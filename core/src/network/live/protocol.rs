//! Live streaming protocol constants and types

use std::time::Duration;

/// ALPN identifier for live streaming connections (MOQ over QUIC)
pub const LIVE_ALPN: &[u8] = b"harbor/live/0";

/// Maximum TTL for stream signaling packets stored on harbor (6 hours)
pub const STREAM_SIGNAL_TTL: Duration = Duration::from_secs(6 * 3600);

/// Error type for live streaming operations
#[derive(Debug)]
pub enum LiveError {
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

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveError::Signaling(e) => write!(f, "signaling error: {e}"),
            LiveError::Moq(e) => write!(f, "MOQ error: {e}"),
            LiveError::RequestNotFound => write!(f, "stream request not found"),
            LiveError::StreamEnded => write!(f, "stream already ended"),
            LiveError::Connection(e) => write!(f, "connection error: {e}"),
            LiveError::Database(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for LiveError {}

impl From<moq_lite::Error> for LiveError {
    fn from(e: moq_lite::Error) -> Self {
        LiveError::Moq(e.to_string())
    }
}
