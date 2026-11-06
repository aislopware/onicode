//! The link to a Mac's desktopd: dialling it, the control stream's frames, the datagram reader
//! and the feedback uplink a stream task sends on.
//!
//! desktopd listens on its own QUIC endpoint on the tailnet (`onicode_desktop_link::PORT`), so
//! a desktop link is its own connection, plaintext QUIC admitted by tailscale whois like every
//! onicode link. Its hello carries `onicode_desktop_link::CAP_DESKTOP`.
//!
//! Ported from slopty's client `link.rs` (MIT, same authors), reduced to what a desktop needs:
//! no terminals, tunnels or transfers ride it.

use futures::FutureExt as _;
use onicode_desktop_link::broker::DesktopEndpoint;
use onicode_desktop_link::{CAP_DESKTOP, Down, MAX_FRAME, Up};
use onicode_net::hello::Session;
use onicode_net::stream::channel;
use onicode_net::{Link, StreamRx, StreamTx};
use tokio::sync::mpsc;

use crate::router::ScreenRouter;
use crate::screen::{Delivery, Uplink};
use crate::{DesktopClient, DesktopError};

/// Datagrams taken off the link per wake, at most: a read's worth goes through the router
/// under one lock.
const BATCH: usize = 64;

/// Dial desktopd at the first of `endpoint`'s addresses that answers, as a desktop.
///
/// # Errors
///
/// [`DesktopError::NoAddress`] for an endpoint with none; [`DesktopError::NotADesktop`] when
/// what answered lacks the desktop capability; the last dial's error otherwise.
pub(crate) async fn dial(
    client: &DesktopClient,
    endpoint: &DesktopEndpoint,
) -> Result<Session, DesktopError> {
    let mut last = DesktopError::NoAddress;
    for &addr in &endpoint.addrs {
        match client.endpoint().dial(addr).await {
            Ok(peer) if peer.session.peer.has(CAP_DESKTOP) => return Ok(peer.session),
            Ok(peer) => {
                peer.session
                    .link
                    .close(onicode_net::close::PROTOCOL, b"not a desktop");
                last = DesktopError::NotADesktop(addr);
            }
            Err(e) => {
                tracing::debug!(%addr, error = %e, "desktop dial failed");
                last = DesktopError::Peer(e);
            }
        }
    }
    Err(last)
}

/// Send `up` on the control stream; `false` once the stream has ended.
pub(crate) fn send(control: &StreamTx, up: &Up) -> bool {
    control
        .send_frame(&onicode_desktop_link::encode(up))
        .is_ok()
}

/// Read desktopd's frames into `downs` until the stream ends or a frame does not read. Its
/// own task, so a slow frame holds up neither input nor the datagrams.
pub(crate) async fn read_control(mut control: StreamRx, downs: mpsc::UnboundedSender<Down>) {
    control.set_max_frame(MAX_FRAME);
    loop {
        let frame = match control.frame().await {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, "desktop control stream ended");
                return;
            }
        };
        match onicode_desktop_link::decode::<Down>(&frame) {
            Ok(down) => {
                if downs.send(down).is_err() {
                    return;
                }
            }
            Err(e) => tracing::warn!(error = %e, "an unreadable desktop frame: skipped"),
        }
    }
}

/// Route every `MEDIA` datagram the link brings through `router`: one wait, then whatever else
/// is ready already, up to [`BATCH`], stamped with the wake's time. Ends with the link.
pub(crate) async fn read_datagrams(link: Link, router: ScreenRouter) {
    let mut batch = Vec::with_capacity(BATCH);
    loop {
        let Ok((first_channel, first)) = link.recv_datagram().await else {
            return;
        };
        let now = crate::instant();
        if first_channel == channel::MEDIA {
            batch.push(first);
        }
        let mut gone = false;
        while batch.len() < BATCH {
            match link.recv_datagram().now_or_never() {
                Some(Ok((channel::MEDIA, datagram))) => batch.push(datagram),
                Some(Ok(_)) => {}
                Some(Err(_)) => {
                    gone = true;
                    break;
                }
                None => break,
            }
        }
        router.route_many(batch.drain(..), now);
        if gone {
            return;
        }
    }
}

/// Feedback for a stream task, as `FEEDBACK` datagrams on `link`.
pub(crate) fn uplink(link: &Link) -> Uplink {
    Uplink {
        feedback: feedback(link),
        rtt: {
            let link = link.clone();
            Box::new(move || link.rtt())
        },
    }
}

/// Sends one `FEEDBACK` datagram on `link`.
pub(crate) fn feedback(link: &Link) -> Box<dyn Fn(bytes::Bytes) -> Delivery + Send> {
    let link = link.clone();
    Box::new(
        move |bytes| match link.datagram(channel::FEEDBACK, &bytes) {
            Ok(()) => Delivery::Sent,
            Err(_) if link.is_closed() => Delivery::Gone,
            Err(_) => Delivery::Dropped,
        },
    )
}
