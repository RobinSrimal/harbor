//! Network layer for Harbor Protocol
//!
//! Contains:
//! - DHT: Kademlia-style distributed hash table for peer discovery
//! - Send: Core messaging protocol for packet delivery and receipts
//! - Harbor: Offline message storage and retrieval
//! - Share: P2P file sharing for files ≥512 KB
//! - Sync: CRDT synchronization for collaborative documents
//! - Service: Common trait and dependencies for all services
//!
//! Each protocol has its own ALPN and connection pool.

pub mod pool;
pub mod service;
pub mod dht;
pub mod harbor;
pub mod send;
pub mod share;
pub mod sync;

// Re-export commonly used items
pub use service::ServiceDeps;
pub use harbor::{HarborError, HarborMessage, HarborService, HARBOR_ALPN};
pub use send::{Receipt, SendConfig, SendMessage, SendService, SEND_ALPN};
pub use share::{
    FileAnnouncement, CanSeed, ShareConfig, ShareError, ShareMessage, ShareService, SHARE_ALPN,
};
pub use sync::{
    SyncMessage, SyncMessageType, SyncUpdate, InitialSyncRequest, InitialSyncResponse,
    DecodeError as SyncDecodeError, SYNC_MESSAGE_PREFIX, SYNC_ALPN,
};

// Topic message types (payload format for Send packets)
pub use send::{
    get_verification_mode_from_payload,
    JoinMessage,
    LeaveMessage,
    TopicMessage,
    TopicMessageType,
    TopicMessageDecodeError,
    FileAnnouncementMessage,
    CanSeedMessage,
    SyncUpdateMessage,
};


// Sample files are kept for reference but not compiled
// See src/network/connection_pool_sample.rs.bak and src/network/dht/dht_sample.rs.bak
