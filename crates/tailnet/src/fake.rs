//! A stand-in `LocalAPI` for tests: loopback TCP, answering from a function of the request's
//! path and query, and recording what it was asked.
//!
//! Ported from slopty's `slopty-tailnet` `fake.rs` (MIT).

use std::convert::Infallible;
use std::net::Ipv4Addr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::AUTHORIZATION;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use tokio::net::TcpListener;

use crate::{LocalApi, Location};

/// The token the fake daemon is reached with.
pub const TOKEN: &str = "abc";

/// Each request the fake daemon took: its path and query, and its `Authorization` header.
pub type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// Serve `answer(path_and_query) -> (status, body)` on a loopback port until the runtime
/// ends; the [`LocalApi`] that reaches it, and the requests it takes as they come.
///
/// # Errors
/// When no loopback port can be bound.
pub async fn daemon(answer: fn(&str) -> (u16, String)) -> std::io::Result<(LocalApi, Seen)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            let service = service_fn(move |req: Request<Incoming>| {
                let path = req
                    .uri()
                    .path_and_query()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let auth = req
                    .headers()
                    .get(AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .map(ToOwned::to_owned);
                log.lock().push((path.clone(), auth));
                let (code, body) = answer(&path);
                let mut response = Response::new(Full::new(Bytes::from(body)));
                *response.status_mut() =
                    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                async move { Ok::<_, Infallible>(response) }
            });
            tokio::spawn(
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service),
            );
        }
    });
    Ok((
        LocalApi::at(Location::Tcp {
            port,
            token: TOKEN.into(),
        }),
        seen,
    ))
}
