//! Network layer for Harbor Protocol
//!
//! Contains:
//! - DHT: Kademlia-style distributed hash table for peer discovery
//! - Send: Core messaging protocol for packet delivery and receipts
//! - Harbor: Offline message storage and retrieval
//! - Membership: Topic membership, member discovery, Join/Leave messages
//!
//! Each protocol has its own ALPN and connection pool.

pub mod dht;
pub mod harbor;
pub mod membership;
pub mod send;

// Re-export commonly used items
pub use harbor::{HarborError, HarborMessage, HarborService, HARBOR_ALPN};
pub use send::{Receipt, SendConfig, SendMessage, SendService, SEND_ALPN};

// Membership module re-exports (Tier 1: Join/Leave messages, discovery)
pub use membership::{
    // Messages
    get_verification_mode_from_payload,
    JoinMessage,
    LeaveMessage,
    TopicMessage,
    // Wildcard for membership sync
    is_wildcard_recipient,
    WILDCARD_RECIPIENT,
};

// Sample files are kept for reference but not compiled
// See src/network/connection_pool_sample.rs.bak and src/network/dht/dht_sample.rs.bak
