//! Stream service - MOQ-based real-time media transport
//!
//! Provides topic-scoped streaming with explicit acceptance.
//! Application layer handles encoding/decoding; this module handles transport.
//!
//! Architecture:
//! - Signaling (request/accept/reject) flows through SendService
//! - Media transport uses moq-lite over dedicated QUIC connections (STREAM_ALPN)
//! - Sessions are wrapped in StreamSession for topic scoping

pub mod protocol;
pub mod service;
pub mod session;

pub use protocol::{STREAM_ALPN, STREAM_SIGNAL_TTL, StreamError};
pub use service::StreamService;
pub use session::StreamSession;

/// Zero topic_id sentinel value used for DM streams (no topic context)
pub const ZERO_TOPIC_ID: [u8; 32] = [0u8; 32];

/// Convert a topic_id to Option: zero sentinel → None, otherwise Some
pub fn optional_topic_id(topic_id: &[u8; 32]) -> Option<[u8; 32]> {
    if *topic_id == ZERO_TOPIC_ID { None } else { Some(*topic_id) }
}
