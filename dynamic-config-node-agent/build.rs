//! Generates the CSI client and server stubs from the vendored spec.
//!
//! The proto is the Container Storage Interface's own, vendored at a
//! release tag rather than fetched: a build that reaches the network is a
//! build that fails in an air-gapped mirror, and the wire format of a
//! stable API is exactly the kind of thing to pin.
//!
//! Only the services a **node plugin** implements are used — Identity and
//! Node. The Controller, GroupController and SnapshotMetadata services are
//! compiled and ignored: this driver has no controller, which is what
//! `CSIDriver.spec.attachRequired: false` in the manifest says out loud.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/csi.proto");

    tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .compile_protos(&["proto/csi.proto"], &["proto"])?;

    Ok(())
}
