//! Rows dragged out of the window. This Mac's rows go as real paths; another
//! device's go as file promises, each copied to the folder Finder names by a
//! transfer into this Mac's engine.

use std::path::PathBuf;
use std::pin::pin;

use anyhow::{Context as _, anyhow, bail};
use futures::FutureExt as _;
use gpui::{ExternalDragPayload, FileDragPaths, FilePromise, PromiseProgress};
use onicode_proto::files::{
    ConflictPolicy, StartTransferRequest, TransferAction, TransferFrom, TransferState, TransferTo,
};

use super::ExplorerDrag;
use super::client::{ExplorerClient, TransferFrame};
use crate::state::AppState;

/// The type Finder gets for a promised folder; files are plain data.
const FOLDER_UTI: &str = "public.folder";
const DATA_UTI: &str = "public.data";

/// What the platform drags when `drag` leaves the window.
pub(crate) fn external_payload(
    drag: &ExplorerDrag,
    state: &AppState,
) -> Option<ExternalDragPayload> {
    if drag.items.is_empty() {
        return None;
    }
    let local = state.local_device_id.clone()?;
    if drag.device_id == local {
        return Some(ExternalDragPayload::Files(FileDragPaths::new(
            drag.items
                .iter()
                .map(|item| (PathBuf::from(&item.path), item.is_dir)),
        )));
    }
    let client = ExplorerClient::new(state.engine()?.clone(), Some(local.clone()));
    Some(ExternalDragPayload::Promises(
        drag.items
            .iter()
            .map(|item| {
                let client = client.clone();
                let source = drag.device_id.clone();
                let local = local.clone();
                let path = item.path.clone();
                FilePromise::new(
                    item.name.clone(),
                    if item.is_dir { FOLDER_UTI } else { DATA_UTI },
                    move |destination, progress| {
                        fetch(
                            client.clone(),
                            source.clone(),
                            path.clone(),
                            local.clone(),
                            destination,
                            progress,
                        )
                        .boxed()
                    },
                )
            })
            .collect(),
    ))
}

/// Copies `path` from `source` into the folder holding `destination` on this
/// Mac, reporting bytes to Finder, until the job is done, fails or Finder
/// cancels it.
async fn fetch(
    client: ExplorerClient,
    source: String,
    path: String,
    local: String,
    destination: PathBuf,
    progress: PromiseProgress,
) -> anyhow::Result<()> {
    let dir = destination
        .parent()
        .context("the promised file has no folder")?
        .to_string_lossy()
        .into_owned();
    // Watch first: the job's updates queue up while it starts.
    let mut updates = client.watch_transfers(&local).await?;
    let request = StartTransferRequest {
        from: TransferFrom {
            device_id: source,
            paths: vec![path],
        },
        to: TransferTo {
            device_id: local.clone(),
            dir,
        },
        // Finder names the file before the drop, so nothing asks here.
        conflict: ConflictPolicy::KeepBoth,
    };
    let transfer_id = client.start_transfer(&request).await?;
    let mut cancelled = progress.cancelled();
    loop {
        let next = {
            let recv = pin!(updates.recv());
            match futures::future::select(&mut cancelled, recv).await {
                futures::future::Either::Left(((), _)) => None,
                futures::future::Either::Right((value, _)) => Some(value),
            }
        };
        let Some(value) = next else {
            client
                .control(&local, transfer_id, TransferAction::Cancel)
                .await
                .ok();
            bail!("cancelled");
        };
        let Some(value) = value else {
            bail!("the transfer watch ended");
        };
        let updates_in = match TransferFrame::parse(value) {
            Some(TransferFrame::List(list)) => list,
            Some(TransferFrame::One(one)) => vec![*one],
            None => continue,
        };
        for update in updates_in
            .iter()
            .filter(|update| update.transfer_id == transfer_id)
        {
            progress.set_total(update.bytes);
            progress.set_completed(update.bytes_done);
            match update.state {
                TransferState::Done => return Ok(()),
                TransferState::Failed => {
                    return Err(anyhow!(
                        update.error.clone().unwrap_or_else(|| "failed".into())
                    ));
                }
                TransferState::Cancelled => bail!("cancelled"),
                TransferState::Queued
                | TransferState::Running
                | TransferState::Paused
                | TransferState::Conflict => {}
            }
        }
    }
}
