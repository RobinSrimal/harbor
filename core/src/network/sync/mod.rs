//! Sync protocol - CRDT synchronization for collaborative documents
//!
//! Provides real-time sync of Loro CRDT documents within topics.
//!
//! Messages:
//! - Update: Delta updates (via Send protocol, batched)
//! - InitialSyncRequest: New member requests full state
//! - InitialSyncResponse: Full snapshot (via direct connection)

pub mod protocol;
pub mod service;

// Re-export commonly used items
pub use protocol::{
    DecodeError, InitialSyncRequest, InitialSyncResponse, SYNC_ALPN, SYNC_MESSAGE_PREFIX,
    SyncMessage, SyncMessageType, SyncUpdate,
};
pub use service::{
    ProcessSyncError, SyncConfig, SyncService, encode_dm_sync_response, encode_sync_response,
};
