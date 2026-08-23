//! The webhook's pure half, importable by the golden tests. `main.rs`
//! is transport over this.

#![forbid(unsafe_code)]

mod annotations;
pub mod classes;
mod patch;

pub mod installation_file;

pub use annotations::{
    installation, of_pod, of_pod_with, verify_installation, Installation, Mode, Request,
    ScopedNames, INJECTED, PREFIX, STATUS,
};
/// The annotation contract as data: every key, whether it takes a `.name`
/// suffix, and whether it has been retired.
///
/// Public so the documentation can be checked against it rather than kept
/// in step by hand — see `tests/registry.rs`.
pub mod registry {
    pub use crate::annotations::{deprecations, is_known, is_per_render, AnnotationSpec, REGISTRY};
}
pub use patch::{
    admission_response, admission_response_with, admission_response_with_classes, patches_for,
    CONFLICT, MALFORMED, PINNED, POLICY,
};
