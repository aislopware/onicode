//! The client's error surface — coarse, user-presentable categories. Transport
//! detail rides the message string.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    /// Unknown chat / project / device / queue row.
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The host device could not be reached over its link (not on the tailnet
    /// yet, the engine not found or away, or the call timed out).
    #[error("host unavailable: {0}")]
    HostUnavailable(String),
    /// The host is reachable but does not advertise the needed capability.
    #[error("not supported by the host: {0}")]
    Unsupported(String),
    /// The host answered with an error (bad request, git failure, …).
    #[error("host error: {0}")]
    HostError(String),
    #[error("storage: {0}")]
    Storage(String),
    /// A path this build doesn't support yet.
    #[error("not implemented yet: {0}")]
    NotImplemented(String),
    /// The client was shut down while the call was in flight.
    #[error("client is shut down")]
    Closed,
    #[error("{0}")]
    Internal(String),
}

impl From<onicode_doc::DocError> for ClientError {
    fn from(err: onicode_doc::DocError) -> Self {
        ClientError::Internal(err.to_string())
    }
}

pub type Result<T, E = ClientError> = std::result::Result<T, E>;
