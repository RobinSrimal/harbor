//! Share protocol data layer
//!
//! Handles persistence for file sharing:
//! - Blob metadata in database
//! - Blob file storage on disk

pub mod blob_store;
pub mod metadata;

// Re-export commonly used items
pub use blob_store::{BlobStore, default_blob_path};
pub use metadata::{
    BlobMetadata, BlobState, CHUNK_SIZE, SectionTrace, add_blob_recipient, delete_blob, get_blob,
    get_blobs_for_scope, get_section_peer_suggestion, get_section_traces, init_blob_sections,
    insert_blob, is_distribution_complete, mark_blob_complete, mark_recipient_complete,
    record_peer_can_seed, record_section_received,
};
