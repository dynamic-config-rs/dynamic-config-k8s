//! The webhook's pure half, importable by the golden tests. `main.rs`
//! is transport over this.

#![forbid(unsafe_code)]

mod annotations;
mod patch;

pub use annotations::{of_pod, Mode, Request, PREFIX};
pub use patch::{admission_response, patches_for};
