//! The frame IO sync sessions run over: the halves of a peer stream, or in-memory pipes.

use std::future::Future;

use bytes::Bytes;

/// The stream failed or ended mid-exchange.
#[derive(Debug, Clone, thiserror::Error)]
#[error("stream: {0}")]
pub struct FrameError(pub String);

/// The sending half a session writes frames to.
pub trait FrameTx: Send + Sync {
    /// Queues one frame; never blocks.
    ///
    /// # Errors
    ///
    /// [`FrameError`] once the stream has ended.
    fn send(&self, payload: &[u8]) -> Result<(), FrameError>;

    /// Resolves once at most `below` bytes wait to be sent, or the stream has ended.
    fn room(&self, below: usize) -> impl Future<Output = ()> + Send + 'static;
}

/// The receiving half a session reads frames from.
pub trait FrameRx: Send {
    /// The next frame; `None` at a clean end of the stream.
    fn recv(&mut self) -> impl Future<Output = Result<Option<Bytes>, FrameError>> + Send;
}

impl FrameTx for onicode_net::StreamTx {
    fn send(&self, payload: &[u8]) -> Result<(), FrameError> {
        self.send_frame(payload)
            .map_err(|e| FrameError(e.to_string()))
    }

    fn room(&self, below: usize) -> impl Future<Output = ()> + Send + 'static {
        Self::room(self, below)
    }
}

impl FrameRx for onicode_net::StreamRx {
    async fn recv(&mut self) -> Result<Option<Bytes>, FrameError> {
        self.frame().await.map_err(|e| FrameError(e.to_string()))
    }
}

/// In-memory frame pipes, for tests here and in the crates that drive sessions.
pub mod pipe {
    use std::future::Future;

    use bytes::Bytes;
    use tokio::sync::mpsc;

    use super::{FrameError, FrameRx, FrameTx};

    /// The sending end of a pipe.
    #[derive(Debug, Clone)]
    pub struct PipeTx(mpsc::UnboundedSender<Bytes>);

    /// The receiving end of a pipe.
    #[derive(Debug)]
    pub struct PipeRx(mpsc::UnboundedReceiver<Bytes>);

    /// One direction of a stream.
    #[must_use]
    pub fn pipe() -> (PipeTx, PipeRx) {
        let (tx, rx) = mpsc::unbounded_channel();
        (PipeTx(tx), PipeRx(rx))
    }

    impl FrameTx for PipeTx {
        fn send(&self, payload: &[u8]) -> Result<(), FrameError> {
            self.0
                .send(Bytes::copy_from_slice(payload))
                .map_err(|_| FrameError("pipe closed".into()))
        }

        fn room(&self, _below: usize) -> impl Future<Output = ()> + Send + 'static {
            std::future::ready(())
        }
    }

    impl FrameRx for PipeRx {
        async fn recv(&mut self) -> Result<Option<Bytes>, FrameError> {
            Ok(self.0.recv().await)
        }
    }
}
