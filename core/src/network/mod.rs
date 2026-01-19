//! Network layer for Harbor Protocol
//!
//! Contains:
//! - DHT: Kademlia-style distributed hash table for peer discovery
//! - Send: Core messaging protocol for packet delivery and receipts
//! - Harbor: Offline message storage and retrieval
//! - Membership: Topic membership, member discovery, Join/Leave messages
//! - Share: P2P file sharing for files ≥512 KB
//! - Sync: CRDT synchronization for collaborative documents
//!
//! Each protocol has its own ALPN and connection pool.

pub mod dht;
pub mod harbor;
pub mod membership;
pub mod send;
pub mod share;
pub mod sync;

// Re-export commonly used items
pub use harbor::{HarborError, HarborMessage, HarborService, HARBOR_ALPN};
pub use send::{Receipt, SendConfig, SendMessage, SendService, SEND_ALPN};
pub use share::{
    FileAnnouncement, CanSeed, ShareConfig, ShareError, ShareMessage, ShareService, SHARE_ALPN,
};
pub use sync::{
    SyncMessage, SyncMessageType, SyncUpdate, InitialSyncRequest, InitialSyncResponse,
    DecodeError as SyncDecodeError, SYNC_MESSAGE_PREFIX, SYNC_ALPN,
};

// Membership module re-exports (Tier 1: Join/Leave/FileAnnouncement/CanSeed messages, discovery)
pub use membership::{
    // Messages
    get_verification_mode_from_payload,
    JoinMessage,
    LeaveMessage,
    TopicMessage,
    // Share-related control messages (proper TopicMessage variants)
    FileAnnouncementMessage,
    CanSeedMessage,
    // Wildcard for membership sync
    is_wildcard_recipient,
    WILDCARD_RECIPIENT,
};

// Sample files are kept for reference but not compiled
// See src/network/connection_pool_sample.rs.bak and src/network/dht/dht_sample.rs.bak
