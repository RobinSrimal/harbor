//! Share Service - Core infrastructure
//!
//! This module contains the core ShareService struct, configuration, error types,
//! and shared utility methods. Business logic is organized in:
//! - `distribute.rs` - File distribution (sharing)
//! - `acquire.rs` - File acquisition (pulling)
//! - `incoming.rs` - Incoming message handlers

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::info;

use crate::data::{
    BlobMetadata, BlobState, BlobStore, LocalIdentity, SectionTrace, get_blob, get_blobs_for_scope,
    get_section_traces,
};
use crate::network::connect::Connector;
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

/// Share service for the Harbor protocol
impl std::fmt::Debug for ShareService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShareService").finish_non_exhaustive()
    }
}

pub struct ShareService {
    /// Connector for outgoing QUIC connections
    pub(super) connector: Arc<Connector>,
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
}

impl ShareService {
    /// Create a new Share service
    pub fn new(
        connector: Arc<Connector>,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        blob_store: Arc<BlobStore>,
        config: ShareConfig,
        send_service: Option<Arc<SendService>>,
        connection_gate: Option<Arc<ConnectionGate>>,
    ) -> Self {
        Self {
            connector,
            identity,
            db,
            blob_store,
            config,
            send_service: Arc::new(RwLock::new(send_service)),
            connection_gate,
        }
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
        // In sync contexts, fail fast if the async DB mutex is already held.
        let metadata = {
            let db_lock = try_lock_db_for_sync_export(&self.db)?;
            get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?
        };

        ensure_blob_complete(metadata.state)?;

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

        ensure_blob_complete(metadata.state)?;

        let blob_store = self.blob_store.clone();
        let hash_copy = *hash;
        let dest_path_owned = dest_path.to_owned();
        let dest_path_display = dest_path.display().to_string();

        tokio::task::spawn_blocking(move || blob_store.export_file(&hash_copy, &dest_path_owned))
            .await
            .map_err(map_export_join_error)??;

        info!(
            hash = hex::encode(&hash[..8]),
            path = %dest_path_display,
            "blob exported"
        );

        Ok(())
    }
}

fn map_export_join_error(e: tokio::task::JoinError) -> ShareError {
    ShareError::Protocol(format!("export task failed: {}", e))
}

fn try_lock_db_for_sync_export<'a>(
    db: &'a Mutex<DbConnection>,
) -> Result<tokio::sync::MutexGuard<'a, DbConnection>, ShareError> {
    db.try_lock().map_err(|_| {
        ShareError::Protocol(
            "database is busy; use export_blob_async in async contexts".to_string(),
        )
    })
}

fn ensure_blob_complete(state: BlobState) -> Result<(), ShareError> {
    if state == BlobState::Complete {
        Ok(())
    } else {
        Err(ShareError::Protocol(
            "Cannot export incomplete blob".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::start_memory_db;

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

    #[test]
    fn test_ensure_blob_complete_rejects_partial() {
        let err = ensure_blob_complete(BlobState::Partial).expect_err("partial blob must fail");
        assert!(matches!(err, ShareError::Protocol(_)));
        assert!(err.to_string().contains("incomplete"));
    }

    #[tokio::test]
    async fn test_try_lock_db_for_sync_export_busy() {
        let conn = start_memory_db().expect("memory db");
        let db = Mutex::new(conn);
        let _guard = db.lock().await;
        let err = try_lock_db_for_sync_export(&db).expect_err("busy lock should fail fast");
        assert!(matches!(err, ShareError::Protocol(_)));
        assert!(err.to_string().contains("database is busy"));
    }

    #[tokio::test]
    async fn test_export_join_error_maps_to_protocol() {
        let join_err = tokio::task::spawn_blocking(|| panic!("boom"))
            .await
            .expect_err("panic should surface as JoinError");
        let mapped = map_export_join_error(join_err);
        assert!(matches!(mapped, ShareError::Protocol(_)));
        assert!(mapped.to_string().contains("export task failed"));
    }
}
