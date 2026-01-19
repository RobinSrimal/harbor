//! Sync protocol data layer
//!
//! Handles persistence for CRDT sync:
//! - WAL (Write-Ahead Log) for durability - per operation
//! - Snapshots for fast recovery - periodic
//!
//! File structure:
//! ```text
//! .harbor_sync/
//! ├── {topic_id_hex}.loro      # Periodic snapshot (shallow)
//! ├── {topic_id_hex}.wal       # Write-ahead log (deltas)
//! └── ...
//! ```

pub mod persistence;

// Re-export commonly used items
pub use persistence::{
    SyncStore, SyncStoreError, default_sync_path,
};

