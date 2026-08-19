//! The webhook's pure half, importable by the golden tests. `main.rs`
//! is transport over this.

#![forbid(unsafe_code)]

mod annotations;
mod patch;

pub use annotations::{
    of_pod, of_pod_with, verify_installation, Installation, Mode, Request, ScopedNames, PREFIX,
};
pub use patch::{admission_response, admission_response_with, patches_for};
