//! The agent's machinery as a library, so the operator reconciles with
//! the SAME source construction and rendering the sidecar uses — one
//! implementation, no drift between the two paths a document can take
//! into a pod.

#![forbid(unsafe_code)]

pub mod metrics;
pub mod render;
pub mod sidecar;
pub mod sources;
pub mod spec;
