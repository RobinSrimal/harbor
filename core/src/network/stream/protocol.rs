//! Stream protocol constants and types

use std::time::Duration;

/// ALPN identifier for stream connections (MOQ over QUIC)
pub const STREAM_ALPN: &[u8] = b"harbor/stream/0";

/// Maximum TTL for stream signaling packets stored on harbor (6 hours)
pub const STREAM_SIGNAL_TTL: Duration = Duration::from_secs(6 * 3600);

/// Error type for streaming operations
#[derive(Debug)]
pub enum StreamError {
    /// Failed to send signaling message
    Signaling(String),
    /// MOQ session error
    Moq(String),
    /// Stream request not found
    RequestNotFound,
    /// Stream already ended
    StreamEnded,
    /// Connection error
    Connection(String),
    /// Database error
    Database(String),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::Signaling(e) => write!(f, "signaling error: {e}"),
            StreamError::Moq(e) => write!(f, "MOQ error: {e}"),
            StreamError::RequestNotFound => write!(f, "stream request not found"),
            StreamError::StreamEnded => write!(f, "stream already ended"),
            StreamError::Connection(e) => write!(f, "connection error: {e}"),
            StreamError::Database(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for StreamError {}

impl From<moq_lite::Error> for StreamError {
    fn from(e: moq_lite::Error) -> Self {
        StreamError::Moq(e.to_string())
    }
}
