//! Share protocol - P2P file sharing for files ≥512 KB
//!
//! Implements pull-based distribution with:
//! - Source-first discovery
//! - Section-level replication traces
//! - Adaptive splitting based on available peers
//!
//! Architecture:
//! - `protocol.rs` - Wire format and message types
//! - `service.rs` - Core ShareService struct, config, shared methods
//! - `distribute.rs` - File distribution (sharing)
//! - `acquire.rs` - File acquisition (pulling)
//! - `incoming.rs` - Incoming message handlers
//!
//! Messages:
//! - FileAnnouncement: Broadcast via Send when sharing starts
//! - CanSeed: Broadcast via Send when peer reaches 100%
//! - Direct messages: ChunkMapRequest, ChunkRequest, Bitfield, PeerSuggestion

pub mod acquire;
pub mod distribute;
pub mod incoming;
pub mod protocol;
pub mod service;

// Re-export commonly used items
pub use protocol::{
    BitfieldMessage, CanSeed, ChunkAck, ChunkMapRequest, ChunkMapResponse, ChunkRequest,
    ChunkResponse, DecodeError, FileAnnouncement, InitialRecipient, PeerChunks, PeerSuggestion,
    ShareMessage, ShareMessageType, SHARE_ALPN,
};
pub use service::{ProcessShareError, ShareConfig, ShareError, ShareService, ShareStatus};

