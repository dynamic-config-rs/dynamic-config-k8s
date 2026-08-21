//! The webhook's pure half, importable by the golden tests. `main.rs`
//! is transport over this.

#![forbid(unsafe_code)]

mod annotations;
mod patch;

pub mod installation_file;

pub use annotations::{
    of_pod, of_pod_with, verify_installation, Installation, Mode, Request, ScopedNames, INJECTED,
    PREFIX, STATUS,
};
pub use patch::{
    admission_response, admission_response_with, patches_for, CONFLICT, MALFORMED, PINNED, POLICY,
};
