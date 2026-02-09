//! Share Service - Core infrastructure
//!
//! This module contains the core ShareService struct, configuration, error types,
//! and shared utility methods. Business logic is organized in:
//! - `distribute.rs` - File distribution (sharing)
//! - `acquire.rs` - File acquisition (pulling)
//! - `incoming.rs` - Incoming message handlers

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::{info};

use crate::data::{
    BlobMetadata, BlobState, BlobStore, LocalIdentity, SectionTrace,
    get_blob, get_blobs_for_scope, get_section_traces,
};
use crate::network::gate::ConnectionGate;
use crate::network::send::SendService;
use crate::protocol::MemberInfo;

use super::protocol::BitfieldMessage;

/// Configuration for the Share service
#[derive(Debug, Clone)]
pub struct ShareConfig {
    /// Maximum concurrent connections per peer
    pub max_connections: usize,
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Chunk transfer timeout
    pub chunk_timeout: Duration,
}

impl Default for ShareConfig {
    fn default() -> Self {
        Self {
            max_connections: 5,
            connect_timeout: Duration::from_secs(10),
            chunk_timeout: Duration::from_secs(30),
        }
    }
}

/// Error during Share operations
#[derive(Debug)]
pub enum ShareError {
    /// File too small for chunked protocol
    FileTooSmall(u64),
    /// File not found
    FileNotFound(String),
    /// Blob not found in database
    BlobNotFound,
    /// IO error
    Io(std::io::Error),
    /// Database error
    Database(String),
    /// Connection error
    Connection(String),
    /// Protocol error
    Protocol(String),
    /// All peers busy
    AllPeersBusy,
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::FileTooSmall(size) => {
                write!(
                    f,
                    "file too small for chunked protocol: {} bytes (min 512 KB)",
                    size
                )
            }
            ShareError::FileNotFound(path) => write!(f, "file not found: {}", path),
            ShareError::BlobNotFound => write!(f, "blob not found"),
            ShareError::Io(e) => write!(f, "IO error: {}", e),
            ShareError::Database(e) => write!(f, "database error: {}", e),
            ShareError::Connection(e) => write!(f, "connection error: {}", e),
            ShareError::Protocol(e) => write!(f, "protocol error: {}", e),
            ShareError::AllPeersBusy => write!(f, "all peers are busy"),
        }
    }
}

impl std::error::Error for ShareError {}

impl From<std::io::Error> for ShareError {
    fn from(e: std::io::Error) -> Self {
        ShareError::Io(e)
    }
}

/// Plan for pulling a blob from peers
#[derive(Debug, Clone)]
pub struct PullPlan {
    /// Chunks we're missing
    pub missing_chunks: Vec<u32>,
    /// Which peers have which chunks
    pub peer_assignments: Vec<PeerAssignment>,
    /// Total chunks in the blob
    pub total_chunks: u32,
}

/// Assignment of chunks to request from a specific peer
#[derive(Debug, Clone)]
pub struct PeerAssignment {
    /// Peer to request from
    pub peer_id: [u8; 32],
    /// Chunks this peer has that we need
    pub chunks: Vec<u32>,
}

/// Plan for announcing a file to topic members
#[derive(Debug, Clone)]
pub struct AnnouncementPlan {
    /// The encoded topic message to broadcast
    pub message_bytes: Vec<u8>,
    /// Recipients for the announcement (with relay URLs)
    pub recipients: Vec<MemberInfo>,
    /// Section push assignments (peer_id, section_id, chunk_start, chunk_end)
    pub section_pushes: Vec<SectionPush>,
}

/// A section to push to a peer
#[derive(Debug, Clone)]
pub struct SectionPush {
    /// Peer to push to
    pub peer_id: [u8; 32],
    /// Section ID
    pub section_id: u8,
    /// First chunk in section
    pub chunk_start: u32,
    /// Last chunk (exclusive) in section
    pub chunk_end: u32,
}

/// Result of processing a received chunk
#[derive(Debug, Clone)]
pub struct ChunkProcessResult {
    /// Whether the chunk was valid and stored
    pub stored: bool,
    /// Whether the blob is now complete
    pub blob_complete: bool,
}

/// Status of a file share operation
#[derive(Debug, Clone)]
pub struct ShareStatus {
    /// Blob hash
    pub hash: [u8; 32],
    /// Display name
    pub display_name: String,
    /// Total size in bytes
    pub total_size: u64,
    /// Total number of chunks
    pub total_chunks: u32,
    /// Number of sections
    pub num_sections: u8,
    /// Current state
    pub state: BlobState,
    /// Chunks we have locally (bitfield)
    pub local_chunks: Vec<bool>,
    /// Number of chunks we have
    pub chunks_complete: u32,
    /// Section traces (who has what sections)
    pub section_traces: Vec<SectionTrace>,
}

/// Error during incoming share message processing
#[derive(Debug)]
pub enum ProcessShareError {
    /// Database error
    Database(String),
    /// Blob not found
    BlobNotFound,
    /// IO error (chunk read/write)
    Io(String),
}

impl std::fmt::Display for ProcessShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessShareError::Database(e) => write!(f, "database error: {}", e),
            ProcessShareError::BlobNotFound => write!(f, "blob not found"),
            ProcessShareError::Io(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for ProcessShareError {}

/// Active transfer state
#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct ActiveTransfer {
    /// Peer we're transferring to/from
    pub peer_id: [u8; 32],
    /// Connection
    pub connection: Connection,
    /// Chunks being transferred
    pub chunks_in_progress: Vec<u32>,
}

/// Share service for the Harbor protocol
impl std::fmt::Debug for ShareService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareService").finish_non_exhaustive()
    }
}

pub struct ShareService {
    /// Iroh endpoint
    pub(super) endpoint: Endpoint,
    /// Local identity (shared with Protocol)
    pub(super) identity: Arc<LocalIdentity>,
    /// Database connection (shared with Protocol)
    pub(super) db: Arc<Mutex<DbConnection>>,
    /// Blob file storage
    pub(super) blob_store: Arc<BlobStore>,
    /// Configuration
    pub(super) config: ShareConfig,
    /// Send service for broadcasting announcements
    pub(super) send_service: Arc<RwLock<Option<Arc<SendService>>>>,
    /// Connection gate for peer authorization
    pub(super) connection_gate: Option<Arc<ConnectionGate>>,
    /// Active outgoing transfers
    pub(super) outgoing_transfers: Arc<RwLock<HashMap<[u8; 32], Vec<ActiveTransfer>>>>,
    /// Active incoming transfers
    pub(super) incoming_transfers: Arc<RwLock<HashMap<[u8; 32], Vec<ActiveTransfer>>>>,
}

impl ShareService {
    /// Create a new Share service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        blob_store: Arc<BlobStore>,
        config: ShareConfig,
        send_service: Option<Arc<SendService>>,
        connection_gate: Option<Arc<ConnectionGate>>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            blob_store,
            config,
            send_service: Arc::new(RwLock::new(send_service)),
            connection_gate,
            outgoing_transfers: Arc::new(RwLock::new(HashMap::new())),
            incoming_transfers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Set the send service (called after ShareService is created)
    pub async fn set_send_service(&self, send_service: Arc<SendService>) {
        *self.send_service.write().await = Some(send_service);
    }

    /// Get the connection gate (for handler to check peer authorization)
    pub fn connection_gate(&self) -> Option<&Arc<ConnectionGate>> {
        self.connection_gate.as_ref()
    }

    /// Get our endpoint ID (public key)
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the identity (for signing/encryption operations)
    pub fn identity(&self) -> &LocalIdentity {
        &self.identity
    }

    /// Get blob store reference
    pub fn blob_store(&self) -> &BlobStore {
        &self.blob_store
    }

    /// Get database reference
    pub fn db(&self) -> &Mutex<DbConnection> {
        &self.db
    }

    /// Get current bitfield for a blob
    pub fn get_bitfield(
        &self,
        hash: &[u8; 32],
        total_chunks: u32,
    ) -> Result<BitfieldMessage, ShareError> {
        let chunks = self.blob_store.get_chunk_bitfield(hash, total_chunks)?;
        Ok(BitfieldMessage::from_chunks(*hash, &chunks))
    }

    /// Check if we have a complete copy of a blob
    pub fn has_complete(&self, hash: &[u8; 32], total_chunks: u32) -> Result<bool, ShareError> {
        Ok(self.blob_store.is_complete(hash, total_chunks)?)
    }

    /// Get active transfer count for a blob
    pub async fn active_transfer_count(&self, hash: &[u8; 32]) -> usize {
        let outgoing = self.outgoing_transfers.read().await;
        let incoming = self.incoming_transfers.read().await;

        outgoing.get(hash).map(|v| v.len()).unwrap_or(0)
            + incoming.get(hash).map(|v| v.len()).unwrap_or(0)
    }

    /// Check if we're at connection capacity
    pub async fn at_capacity(&self, hash: &[u8; 32]) -> bool {
        self.active_transfer_count(hash).await >= self.config.max_connections
    }

    /// Get the status of a shared file
    pub async fn get_status(&self, hash: &[u8; 32]) -> Result<ShareStatus, ShareError> {
        let (metadata, section_traces) = {
            let db_lock = self.db.lock().await;

            let metadata = get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?;

            let traces = get_section_traces(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;

            (metadata, traces)
        };

        // Get local chunk bitfield
        let local_chunks = self
            .blob_store
            .get_chunk_bitfield(hash, metadata.total_chunks)?;
        let chunks_complete = local_chunks.iter().filter(|&&b| b).count() as u32;

        Ok(ShareStatus {
            hash: *hash,
            display_name: metadata.display_name,
            total_size: metadata.total_size,
            total_chunks: metadata.total_chunks,
            num_sections: metadata.num_sections,
            state: metadata.state,
            local_chunks,
            chunks_complete,
            section_traces,
        })
    }

    /// List all shared files for a scope (topic or endpoint)
    pub async fn list_blobs(&self, scope_id: &[u8; 32]) -> Result<Vec<BlobMetadata>, ShareError> {
        let db_lock = self.db.lock().await;
        get_blobs_for_scope(&db_lock, scope_id).map_err(|e| ShareError::Database(e.to_string()))
    }

    /// Export a completed blob to a file
    pub fn export_blob(&self, hash: &[u8; 32], dest_path: &Path) -> Result<(), ShareError> {
        // Check blob is complete (synchronous check via try_lock for simplicity)
        // In practice, caller should use async version
        let metadata = {
            // Use blocking lock since we're in a sync context
            let db_lock = futures::executor::block_on(self.db.lock());
            get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?
        };

        if metadata.state != BlobState::Complete {
            return Err(ShareError::Protocol(
                "Cannot export incomplete blob".to_string(),
            ));
        }

        self.blob_store.export_file(hash, dest_path)?;
        Ok(())
    }

    /// Export a completed blob to a file (async version)
    pub async fn export_blob_async(
        &self,
        hash: &[u8; 32],
        dest_path: &Path,
    ) -> Result<(), ShareError> {
        let metadata = {
            let db_lock = self.db.lock().await;
            get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?
        };

        if metadata.state != BlobState::Complete {
            return Err(ShareError::Protocol(
                "Cannot export incomplete blob".to_string(),
            ));
        }

        let blob_store = self.blob_store.clone();
        let hash_copy = *hash;
        let dest_path_owned = dest_path.to_owned();
        let dest_path_display = dest_path.display().to_string();

        tokio::task::spawn_blocking(move || blob_store.export_file(&hash_copy, &dest_path_owned))
            .await
            .map_err(|e| ShareError::Connection(e.to_string()))??;

        info!(
            hash = hex::encode(&hash[..8]),
            path = %dest_path_display,
            "blob exported"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_share_config_default() {
        let config = ShareConfig::default();
        assert_eq!(config.max_connections, 5);
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
    }

    #[test]
    fn test_share_error_display() {
        let err = ShareError::FileTooSmall(100);
        assert!(err.to_string().contains("100"));

        let err = ShareError::BlobNotFound;
        assert!(err.to_string().contains("not found"));
    }
}
