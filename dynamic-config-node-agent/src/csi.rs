//! The Container Storage Interface, as much of it as a node plugin is.
//!
//! Seven calls. The kubelet asks who this is, what it can do, and then
//! publishes and unpublishes volumes as pods come and go; everything else
//! in the specification belongs to a controller, and this driver has none —
//! which is what `attachRequired: false` on the `CSIDriver` object says.
//!
//! # Idempotence is the whole contract
//!
//! The kubelet retries. It retries a publish that timed out, a publish it
//! never saw the answer to, and an unpublish it interrupted; every one of
//! those has to be safe. So `NodePublishVolume` on a target that is already
//! published succeeds without doing anything, and `NodeUnpublishVolume` on
//! a target nobody knows about succeeds too — refusing it would wedge the
//! pod's deletion on a volume that is already gone.

use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::proto::{
    identity_server::Identity, node_server::Node, GetPluginCapabilitiesRequest,
    GetPluginCapabilitiesResponse, GetPluginInfoRequest, GetPluginInfoResponse,
    NodeExpandVolumeRequest, NodeExpandVolumeResponse, NodeGetCapabilitiesRequest,
    NodeGetCapabilitiesResponse, NodeGetInfoRequest, NodeGetInfoResponse,
    NodeGetVolumeStatsRequest, NodeGetVolumeStatsResponse, NodePublishVolumeRequest,
    NodePublishVolumeResponse, NodeStageVolumeRequest, NodeStageVolumeResponse,
    NodeUnpublishVolumeRequest, NodeUnpublishVolumeResponse, NodeUnstageVolumeRequest,
    NodeUnstageVolumeResponse, ProbeRequest, ProbeResponse,
};

/// The plugin, and the state it keeps for the node it runs on.
#[derive(Clone)]
pub struct Plugin {
    node: String,
    registry: crate::shared::Registry,
    /// What is currently published, so a retry is a no-op and an unpublish
    /// knows what to tear down.
    published: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
}

impl Plugin {
    /// A plugin for `node`, which is the name the kubelet knows it by.
    #[must_use]
    pub fn new(node: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            registry: crate::shared::Registry::new(),
            published: Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new())),
        }
    }

    /// The registry, for the metrics endpoint.
    #[must_use]
    pub fn registry(&self) -> &crate::shared::Registry {
        &self.registry
    }
}

#[tonic::async_trait]
impl Identity for Plugin {
    async fn get_plugin_info(
        &self,
        _request: Request<GetPluginInfoRequest>,
    ) -> Result<Response<GetPluginInfoResponse>, Status> {
        Ok(Response::new(GetPluginInfoResponse {
            name: crate::DRIVER.to_owned(),
            vendor_version: env!("CARGO_PKG_VERSION").to_owned(),
            manifest: std::collections::HashMap::new(),
        }))
    }

    async fn get_plugin_capabilities(
        &self,
        _request: Request<GetPluginCapabilitiesRequest>,
    ) -> Result<Response<GetPluginCapabilitiesResponse>, Status> {
        // Empty, and deliberately: no `CONTROLLER_SERVICE`, because there
        // is no controller — nothing is provisioned, attached or resized.
        // A volume here is a directory the kubelet already made and a
        // document written into it.
        Ok(Response::new(GetPluginCapabilitiesResponse {
            capabilities: Vec::new(),
        }))
    }

    async fn probe(
        &self,
        _request: Request<ProbeRequest>,
    ) -> Result<Response<ProbeResponse>, Status> {
        // No `ready` value: the specification reads an absent one as
        // ready, and this plugin has nothing to be un-ready about. It
        // depends on no store; the volumes do, and a store that is down is
        // a publish that fails rather than a plugin that is broken.
        Ok(Response::new(ProbeResponse { ready: None }))
    }
}

#[tonic::async_trait]
impl Node for Plugin {
    async fn node_get_info(
        &self,
        _request: Request<NodeGetInfoRequest>,
    ) -> Result<Response<NodeGetInfoResponse>, Status> {
        Ok(Response::new(NodeGetInfoResponse {
            node_id: self.node.clone(),
            // No limit: a volume costs a watch and a file, not a device or
            // an attachment slot.
            max_volumes_per_node: 0,
            accessible_topology: None,
        }))
    }

    async fn node_get_capabilities(
        &self,
        _request: Request<NodeGetCapabilitiesRequest>,
    ) -> Result<Response<NodeGetCapabilitiesResponse>, Status> {
        // Empty: no staging, no volume stats, no expansion. The kubelet
        // then calls only publish and unpublish, which is the whole of
        // what this does.
        Ok(Response::new(NodeGetCapabilitiesResponse {
            capabilities: Vec::new(),
        }))
    }

    async fn node_publish_volume(
        &self,
        request: Request<NodePublishVolumeRequest>,
    ) -> Result<Response<NodePublishVolumeResponse>, Status> {
        let request = request.into_inner();
        let target = request.target_path;

        if target.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        // The retry case, and the first thing checked: the kubelet repeats
        // a publish it did not see the answer to, and doing the work twice
        // would start a second watch on the same file.
        {
            let published = self
                .published
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            if published.contains(&target) {
                return Ok(Response::new(NodePublishVolumeResponse {}));
            }
        }

        let volume = crate::publish::Volume::parse(&target, &request.volume_context)
            .map_err(Status::invalid_argument)?;

        std::fs::create_dir_all(&target)
            .map_err(|error| Status::internal(format!("{target}: {error}")))?;

        // The first render happens **before this call returns**, which is
        // the property a CSI volume buys over a sidecar: the kubelet does
        // not start the pod's containers until every volume is published,
        // so the application cannot observe a missing file. There is no
        // init container here because there is nothing for one to do.
        let built = dynamic_config_agent::sources::build(&volume.spec)
            .await
            .map_err(|error| Status::internal(format!("building the source: {error}")))?;

        let fetched = built
            .fetch()
            .await
            .map_err(|error| Status::unavailable(format!("the first fetch: {error}")))?;

        let rendered = dynamic_config_agent::render::render_fetched(&fetched, &volume.spec)
            .map_err(|error| Status::internal(format!("rendering: {error}")))?;

        dynamic_config_agent::render::write_atomically(
            &volume.out,
            &rendered,
            volume.spec.file_mode,
        )
        .map_err(|error| Status::internal(format!("writing: {error}")))?;

        // Then the watch, shared with every other pod on this node reading
        // the same document from the same store under the same credential.
        match self.registry.claim(volume.document.clone(), &target) {
            crate::shared::Claim::Joined => {
                tracing::info!(target, "joined a watch this node already had");
            }
            crate::shared::Claim::Started(stop) => {
                let spec = volume.spec;
                let source = Arc::new(built);

                tokio::spawn(async move {
                    let mut stop = stop;

                    let ending = async move {
                        // `changed` returns when the last reader released
                        // it; the initial value is false and is not a stop.
                        while stop.changed().await.is_ok() {
                            if *stop.borrow() {
                                return;
                            }
                        }
                    };

                    if let Err(error) = dynamic_config_agent::sidecar::run(
                        &spec,
                        &source,
                        spec.watch.unwrap_or(std::time::Duration::from_secs(15)),
                        ending,
                    )
                    .await
                    {
                        tracing::warn!(%error, "a shared watch ended");
                    }
                });

                tracing::info!(target, "started a watch for this node");
            }
        }

        self.published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(target);

        Ok(Response::new(NodePublishVolumeResponse {}))
    }

    async fn node_unpublish_volume(
        &self,
        request: Request<NodeUnpublishVolumeRequest>,
    ) -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        let target = request.into_inner().target_path;

        if target.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        // Unknown is success. The kubelet retries an unpublish it
        // interrupted, and refusing the second one would wedge the pod's
        // deletion on a volume that is already gone.
        if self.registry.release(&target) {
            tracing::info!(target, "the last reader went; the watch ended");
        }

        self.published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&target);

        // The directory is the kubelet's to remove; the file inside it is
        // this driver's. Removing the contents and leaving the mount point
        // is what a node plugin with no real mount does.
        if let Ok(entries) = std::fs::read_dir(&target) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }

        Ok(Response::new(NodeUnpublishVolumeResponse {}))
    }

    // The four a node plugin without staging, stats or expansion answers
    // `UNIMPLEMENTED` to. The kubelet does not call them — it reads
    // `NodeGetCapabilities` first — and answering anything else would be a
    // claim this driver cannot keep.

    async fn node_stage_volume(
        &self,
        _request: Request<NodeStageVolumeRequest>,
    ) -> Result<Response<NodeStageVolumeResponse>, Status> {
        Err(Status::unimplemented("this driver stages nothing"))
    }

    async fn node_unstage_volume(
        &self,
        _request: Request<NodeUnstageVolumeRequest>,
    ) -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        Err(Status::unimplemented("this driver stages nothing"))
    }

    async fn node_get_volume_stats(
        &self,
        _request: Request<NodeGetVolumeStatsRequest>,
    ) -> Result<Response<NodeGetVolumeStatsResponse>, Status> {
        Err(Status::unimplemented(
            "a document has no capacity to report",
        ))
    }

    async fn node_expand_volume(
        &self,
        _request: Request<NodeExpandVolumeRequest>,
    ) -> Result<Response<NodeExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("a document has no size to expand"))
    }
}
