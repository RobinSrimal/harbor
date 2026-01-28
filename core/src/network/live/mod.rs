//! Live streaming service - MOQ-based real-time media transport
//!
//! Provides topic-scoped streaming with explicit acceptance.
//! Application layer handles encoding/decoding; this module handles transport.
//!
//! Architecture:
//! - Signaling (request/accept/reject) flows through SendService
//! - Media transport uses moq-lite over dedicated QUIC connections (LIVE_ALPN)
//! - Sessions are wrapped in LiveSession for topic scoping

pub mod protocol;
pub mod service;
pub mod session;

pub use protocol::{LIVE_ALPN, STREAM_SIGNAL_TTL, LiveError};
pub use service::LiveService;
pub use session::LiveSession;
