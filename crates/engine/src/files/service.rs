//! The Files group as an `onicode-rpc` service: `WatchDir` and the files calls, each wrapped in a
//! `Routed` that names this engine (`None`) and, for a mutating call, its op.
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use onicode_protocol::files::{
    Create, DeletePath, ListDir, ListDrives, Move, Read, SaveAttachments, SaveNoteImages,
    SearchContent, SearchHomeFolders, SearchPaths, Stat, WatchDir, Write,
};
use onicode_protocol::{Call, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use super::hub::FilesHub;
use crate::service::{reply, routed};

pub struct FilesService {
    hub: Arc<FilesHub>,
}

impl FilesService {
    #[must_use]
    pub fn new(hub: Arc<FilesHub>) -> Self {
        Self { hub }
    }
}

/// Every method this service serves.
const METHODS: [&str; 14] = [
    WatchDir::NAME,
    ListDir::NAME,
    Stat::NAME,
    Read::NAME,
    Write::NAME,
    Create::NAME,
    Move::NAME,
    DeletePath::NAME,
    SearchPaths::NAME,
    SearchContent::NAME,
    ListDrives::NAME,
    SearchHomeFolders::NAME,
    SaveAttachments::NAME,
    SaveNoteImages::NAME,
];

impl FilesService {
    /// Whether `method` is one of the Files group's.
    #[must_use]
    pub fn serves(method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for FilesService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            WatchDir::NAME => {
                let (_, body) = routed::<WatchDir>(params)?;
                let items = hub.watch_dir(body).await.map_err(RpcError::Failed)?;
                Ok(RpcReply::Stream(
                    items
                        .filter_map(|item| futures::future::ready(serde_json::to_value(&item).ok()))
                        .boxed(),
                ))
            }
            ListDir::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.list_dir(body).await)
            }
            Stat::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.stat(body).await)
            }
            Read::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.read(body).await)
            }
            Write::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.write(op, body).await)
            }
            Create::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create(op, body).await)
            }
            Move::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.relocate(op, body).await)
            }
            DeletePath::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.delete(op, body).await)
            }
            SearchPaths::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.search_paths(body).await)
            }
            SearchContent::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.search_content(body).await)
            }
            ListDrives::NAME => {
                let (_, ListDrives {}) = routed(params)?;
                reply(hub.list_drives().await)
            }
            SearchHomeFolders::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.search_home_folders(body).await)
            }
            SaveAttachments::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.save_attachments(op, body).await)
            }
            SaveNoteImages::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.save_note_images(op, body).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
