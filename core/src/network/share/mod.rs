//! Share protocol - P2P file sharing for files ≥512 KB
//!
//! Implements pull-based distribution with:
//! - Source-first discovery
//! - Section-level replication traces
//! - Adaptive splitting based on available peers
//!
//! Messages:
//! - FileAnnouncement: Broadcast via Send when sharing starts
//! - CanSeed: Broadcast via Send when peer reaches 100%
//! - Direct messages: ChunkMapRequest, ChunkRequest, Bitfield, PeerSuggestion

pub mod protocol;
pub mod service;

// Re-export commonly used items
pub use protocol::{
    BitfieldMessage, CanSeed, ChunkAck, ChunkMapRequest, ChunkMapResponse, ChunkRequest,
    ChunkResponse, DecodeError, FileAnnouncement, InitialRecipient, PeerChunks, PeerSuggestion,
    ShareMessage, ShareMessageType, SHARE_ALPN,
};
pub use service::{ShareConfig, ShareError, ShareService};

