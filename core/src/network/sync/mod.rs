//! Sync protocol - CRDT synchronization for collaborative documents
//!
//! Provides real-time sync of Loro CRDT documents within topics.
//!
//! Messages:
//! - Update: Delta updates (via Send protocol, batched)
//! - InitialSyncRequest: New member requests full state
//! - InitialSyncResponse: Full snapshot (via direct connection)

pub mod protocol;

// Re-export commonly used items
pub use protocol::{
    SyncMessage, SyncMessageType, SyncUpdate, InitialSyncRequest, InitialSyncResponse,
    DecodeError, SYNC_MESSAGE_PREFIX, SYNC_ALPN,
};

