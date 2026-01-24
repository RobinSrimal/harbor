//! Share Service - manages file distribution
//!
//! The ShareService handles:
//! - Importing files for sharing
//! - Managing chunk transfers
//! - Tracking distribution state
//! - Coordinating with peers

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, NodeAddr, NodeId};
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, trace, warn};

use crate::data::{
    BlobMetadata, BlobState, BlobStore, CHUNK_SIZE, SectionTrace,
    add_blob_recipient, get_blob, get_blobs_for_topic, get_section_peer_suggestion, get_section_traces,
    get_topic_members_with_info, init_blob_sections, insert_blob, mark_blob_complete,
    record_section_received,
};
use super::protocol::{
    BitfieldMessage, ChunkAck, ChunkMapRequest, ChunkMapResponse, ChunkRequest, ChunkResponse,
    FileAnnouncement, InitialRecipient, PeerChunks, PeerSuggestion, ShareMessage, SHARE_ALPN,
};

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
                write!(f, "file too small for chunked protocol: {} bytes (min 512 KB)", size)
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

/// Calculate number of sections based on chunk count
fn calculate_num_sections(total_chunks: u32) -> u8 {
    // Use min(5, chunks) sections, at least 1
    std::cmp::min(5, std::cmp::max(1, total_chunks)) as u8
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
struct ActiveTransfer {
    /// Peer we're transferring to/from
    peer_id: [u8; 32],
    /// Connection
    connection: Connection,
    /// Chunks being transferred
    chunks_in_progress: Vec<u32>,
}

/// Share service for the Harbor protocol
pub struct ShareService {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Local node's endpoint ID
    our_id: [u8; 32],
    /// Blob file storage
    blob_store: Arc<BlobStore>,
    /// Configuration
    config: ShareConfig,
    /// Active outgoing transfers
    outgoing_transfers: Arc<RwLock<HashMap<[u8; 32], Vec<ActiveTransfer>>>>,
    /// Active incoming transfers
    incoming_transfers: Arc<RwLock<HashMap<[u8; 32], Vec<ActiveTransfer>>>>,
}

impl ShareService {
    /// Create a new Share service
    pub fn new(
        endpoint: Endpoint,
        our_id: [u8; 32],
        blob_store: Arc<BlobStore>,
        config: ShareConfig,
    ) -> Self {
        Self {
            endpoint,
            our_id,
            blob_store,
            config,
            outgoing_transfers: Arc::new(RwLock::new(HashMap::new())),
            incoming_transfers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get our endpoint ID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.our_id
    }

    /// Import a file for sharing
    /// 
    /// Returns (hash, FileAnnouncement) to be broadcast via Send
    pub async fn import_file(
        &self,
        source_path: impl AsRef<Path>,
        topic_id: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<([u8; 32], FileAnnouncement), ShareError> {
        let source_path = source_path.as_ref();
        
        // Check file exists and is large enough
        let metadata = std::fs::metadata(source_path)
            .map_err(|_| ShareError::FileNotFound(source_path.display().to_string()))?;
        
        if metadata.len() < CHUNK_SIZE {
            return Err(ShareError::FileTooSmall(metadata.len()));
        }
        
        // Import to blob store
        let (hash, total_size) = self.blob_store.import_file(source_path)?;
        
        let total_chunks = ((total_size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        let display_name = source_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        
        // Store metadata in database
        let num_sections = self.config.max_connections.min(total_chunks as usize).max(1) as u8;
        
        {
            let db_lock = db.lock().await;
            insert_blob(
                &db_lock,
                &hash,
                topic_id,
                &self.our_id,
                &display_name,
                total_size,
                num_sections,
            ).map_err(|e| ShareError::Database(e.to_string()))?;
            
            // Initialize sections
            init_blob_sections(&db_lock, &hash, num_sections, total_chunks)
                .map_err(|e| ShareError::Database(e.to_string()))?;
            
            // Mark ourselves as complete
            mark_blob_complete(&db_lock, &hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;
        }
        
        // Create announcement (initial_recipients will be filled when connections are made)
        let announcement = FileAnnouncement {
            hash,
            source_id: self.our_id,
            total_size,
            total_chunks,
            num_sections,
            display_name,
            merkle_root: hash, // For now, use the file hash as merkle root
            initial_recipients: Vec::new(), // Will be updated after first chunk ACK
        };
        
        info!(
            hash = %hex::encode(&hash[..8]),
            size = total_size,
            chunks = total_chunks,
            sections = num_sections,
            "file imported for sharing"
        );
        
        Ok((hash, announcement))
    }

    /// Start initial distribution to peers
    /// 
    /// Connects to up to max_connections peers and assigns sections.
    /// Returns updated FileAnnouncement with initial_recipients.
    pub async fn start_distribution(
        &self,
        hash: &[u8; 32],
        announcement: &mut FileAnnouncement,
        peers: &[[u8; 32]],
        db: &Mutex<DbConnection>,
    ) -> Result<(), ShareError> {
        let num_peers = peers.len().min(self.config.max_connections);
        if num_peers == 0 {
            return Ok(());
        }
        
        let total_chunks = announcement.total_chunks;
        let num_sections = num_peers as u8;
        let chunks_per_section = (total_chunks + num_sections as u32 - 1) / num_sections as u32;
        
        let mut initial_recipients = Vec::new();
        
        for (i, peer_id) in peers.iter().take(num_peers).enumerate() {
            let section_id = i as u8;
            let chunk_start = section_id as u32 * chunks_per_section;
            let chunk_end = ((section_id as u32 + 1) * chunks_per_section).min(total_chunks);
            
            // Try to connect to peer
            let node_id = NodeId::from_bytes(peer_id)
                .map_err(|e| ShareError::Connection(e.to_string()))?;
            
            match self.connect_to_peer(node_id).await {
                Ok(conn) => {
                    // Add to initial recipients
                    initial_recipients.push(InitialRecipient {
                        endpoint_id: *peer_id,
                        section_id,
                        chunk_start,
                        chunk_end,
                    });
                    
                    // Track recipient in database
                    {
                        let db_lock = db.lock().await;
                        add_blob_recipient(&db_lock, hash, peer_id)
                            .map_err(|e| ShareError::Database(e.to_string()))?;
                    }
                    
                    // Store active transfer
                    {
                        let mut transfers = self.outgoing_transfers.write().await;
                        let entry = transfers.entry(*hash).or_insert_with(Vec::new);
                        entry.push(ActiveTransfer {
                            peer_id: *peer_id,
                            connection: conn,
                            chunks_in_progress: (chunk_start..chunk_end).collect(),
                        });
                    }
                    
                    debug!(
                        peer = %hex::encode(&peer_id[..8]),
                        section = section_id,
                        chunks = ?chunk_start..chunk_end,
                        "assigned section to peer"
                    );
                }
                Err(e) => {
                    warn!(
                        peer = %hex::encode(&peer_id[..8]),
                        error = %e,
                        "failed to connect to peer for distribution"
                    );
                }
            }
        }
        
        announcement.initial_recipients = initial_recipients;
        announcement.num_sections = num_sections;
        
        Ok(())
    }

    /// Send chunks to a peer
    pub async fn send_chunks(
        &self,
        hash: &[u8; 32],
        peer_id: &[u8; 32],
        chunk_indices: &[u32],
        conn: &Connection,
    ) -> Result<(), ShareError> {
        for &chunk_index in chunk_indices {
            // Read chunk from storage
            let chunk_data = self.blob_store.read_chunk(hash, chunk_index)?;
            
            // Create response
            let response = ShareMessage::ChunkResponse(ChunkResponse {
                hash: *hash,
                chunk_index,
                data: chunk_data,
            });
            
            // Send via stream
            let mut send_stream = conn.open_uni().await
                .map_err(|e| ShareError::Connection(e.to_string()))?;
            
            tokio::io::AsyncWriteExt::write_all(&mut send_stream, &response.encode()).await
                .map_err(|e| ShareError::Connection(e.to_string()))?;
            
            send_stream.finish()
                .map_err(|e| ShareError::Connection(e.to_string()))?;
            
            trace!(
                hash = %hex::encode(&hash[..8]),
                chunk = chunk_index,
                peer = %hex::encode(&peer_id[..8]),
                "sent chunk"
            );
        }
        
        Ok(())
    }

    /// Receive and store a chunk
    pub async fn receive_chunk(
        &self,
        hash: &[u8; 32],
        chunk_index: u32,
        data: &[u8],
        total_size: u64,
        from_peer: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<bool, ShareError> {
        // Verify chunk (simplified - would use bao-tree for full verification)
        // For now just verify it's not empty and not too large
        if data.is_empty() || data.len() > CHUNK_SIZE as usize {
            return Err(ShareError::Protocol("invalid chunk size".to_string()));
        }
        
        // Write to storage
        self.blob_store.write_chunk(hash, chunk_index, data, total_size)?;
        
        trace!(
            hash = %hex::encode(&hash[..8]),
            chunk = chunk_index,
            from = %hex::encode(&from_peer[..8]),
            "received chunk"
        );
        
        // Check if this completes a section (for trace recording)
        let total_chunks = ((total_size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        let bitfield = self.blob_store.get_chunk_bitfield(hash, total_chunks)?;
        
        // Check which sections are complete
        {
            let db_lock = db.lock().await;
            if let Ok(traces) = get_section_traces(&db_lock, hash) {
                for trace in traces {
                    // Check if all chunks in section are present
                    let section_complete = (trace.chunk_start..trace.chunk_end)
                        .all(|i| bitfield.get(i as usize).copied().unwrap_or(false));
                    
                    if section_complete && trace.received_from.is_none() {
                        // Record trace
                        let _ = record_section_received(&db_lock, hash, trace.section_id, from_peer);
                        debug!(
                            hash = %hex::encode(&hash[..8]),
                            section = trace.section_id,
                            from = %hex::encode(&from_peer[..8]),
                            "section complete, recorded trace"
                        );
                    }
                }
            }
        }
        
        // Check if blob is complete
        let is_complete = self.blob_store.is_complete(hash, total_chunks)?;
        
        if is_complete {
            let db_lock = db.lock().await;
            mark_blob_complete(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;
            
            info!(
                hash = %hex::encode(&hash[..8]),
                "blob complete"
            );
        }
        
        Ok(is_complete)
    }

    /// Get chunk map for a blob (for responding to ChunkMapRequest)
    pub async fn get_chunk_map(
        &self,
        hash: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<ChunkMapResponse, ShareError> {
        let db_lock = db.lock().await;
        
        // Get section traces (who has what)
        let traces = get_section_traces(&db_lock, hash)
            .map_err(|e| ShareError::Database(e.to_string()))?;
        
        let mut peers = Vec::new();
        
        for trace in traces {
            if let Some(peer_id) = trace.received_from {
                peers.push(PeerChunks {
                    endpoint_id: peer_id,
                    chunk_start: trace.chunk_start,
                    chunk_end: trace.chunk_end,
                });
            }
        }
        
        // Add ourselves if we're the source
        if let Ok(Some(blob)) = get_blob(&db_lock, hash) {
            if blob.source_id == self.our_id && blob.state == BlobState::Complete {
                peers.push(PeerChunks {
                    endpoint_id: self.our_id,
                    chunk_start: 0,
                    chunk_end: blob.total_chunks,
                });
            }
        }
        
        Ok(ChunkMapResponse {
            hash: *hash,
            peers,
        })
    }

    /// Get peer suggestion for a section (when we're busy)
    pub async fn get_peer_suggestion(
        &self,
        hash: &[u8; 32],
        section_id: u8,
        db: &Mutex<DbConnection>,
    ) -> Result<Option<PeerSuggestion>, ShareError> {
        let db_lock = db.lock().await;
        
        if let Some(suggested_peer) = get_section_peer_suggestion(&db_lock, hash, section_id)
            .map_err(|e| ShareError::Database(e.to_string()))?
        {
            Ok(Some(PeerSuggestion {
                hash: *hash,
                section_id,
                suggested_peer,
            }))
        } else {
            Ok(None)
        }
    }

    /// Get current bitfield for a blob
    pub fn get_bitfield(&self, hash: &[u8; 32], total_chunks: u32) -> Result<BitfieldMessage, ShareError> {
        let chunks = self.blob_store.get_chunk_bitfield(hash, total_chunks)?;
        Ok(BitfieldMessage::from_chunks(*hash, &chunks))
    }

    /// Check if we have a complete copy of a blob
    pub fn has_complete(&self, hash: &[u8; 32], total_chunks: u32) -> Result<bool, ShareError> {
        Ok(self.blob_store.is_complete(hash, total_chunks)?)
    }

    /// Connect to a peer
    async fn connect_to_peer(&self, node_id: NodeId) -> Result<Connection, ShareError> {
        let node_addr: NodeAddr = node_id.into();
        
        tokio::time::timeout(
            self.config.connect_timeout,
            self.endpoint.connect(node_addr, SHARE_ALPN),
        )
        .await
        .map_err(|_| ShareError::Connection("timeout".to_string()))?
        .map_err(|e| ShareError::Connection(e.to_string()))
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

    /// Get blob store reference
    pub fn blob_store(&self) -> &BlobStore {
        &self.blob_store
    }

    /// Get recipient IDs for a can-seed announcement
    ///
    /// Returns the endpoint IDs of all topic members to broadcast the announcement to.
    pub async fn get_can_seed_recipients(
        &self,
        topic_id: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<Vec<[u8; 32]>, ShareError> {
        let db_lock = db.lock().await;
        let members = get_topic_members_with_info(&db_lock, topic_id)
            .map_err(|e| ShareError::Database(e.to_string()))?;

        Ok(members.iter().map(|m| m.endpoint_id).collect())
    }

    // ========================================================================
    // High-level operations (moved from protocol/share.rs)
    // ========================================================================

    /// Get the status of a shared file
    pub async fn get_status(
        &self,
        hash: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<ShareStatus, ShareError> {
        let (metadata, section_traces) = {
            let db_lock = db.lock().await;

            let metadata = get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?;

            let traces = get_section_traces(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;

            (metadata, traces)
        };

        // Get local chunk bitfield
        let local_chunks = self.blob_store.get_chunk_bitfield(hash, metadata.total_chunks)?;
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

    /// List all shared files for a topic
    pub async fn list_blobs(
        &self,
        topic_id: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<Vec<BlobMetadata>, ShareError> {
        let db_lock = db.lock().await;
        get_blobs_for_topic(&db_lock, topic_id)
            .map_err(|e| ShareError::Database(e.to_string()))
    }

    /// Export a completed blob to a file
    pub fn export_blob(
        &self,
        hash: &[u8; 32],
        dest_path: &Path,
        db: &Mutex<DbConnection>,
    ) -> Result<(), ShareError> {
        // Check blob is complete (synchronous check via try_lock for simplicity)
        // In practice, caller should use async version
        let metadata = {
            // Use blocking lock since we're in a sync context
            let db_lock = futures::executor::block_on(db.lock());
            get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?
        };

        if metadata.state != BlobState::Complete {
            return Err(ShareError::Protocol("Cannot export incomplete blob".to_string()));
        }

        self.blob_store.export_file(hash, dest_path)?;
        Ok(())
    }

    /// Export a completed blob to a file (async version)
    pub async fn export_blob_async(
        &self,
        hash: &[u8; 32],
        dest_path: &Path,
        db: &Mutex<DbConnection>,
    ) -> Result<(), ShareError> {
        // Check blob is complete
        let metadata = {
            let db_lock = db.lock().await;
            get_blob(&db_lock, hash)
                .map_err(|e| ShareError::Database(e.to_string()))?
                .ok_or(ShareError::BlobNotFound)?
        };

        if metadata.state != BlobState::Complete {
            return Err(ShareError::Protocol("Cannot export incomplete blob".to_string()));
        }

        self.blob_store.export_file(hash, dest_path)?;
        Ok(())
    }

    /// Share a file with topic members
    ///
    /// This is the main share workflow:
    /// 1. Import file to blob store
    /// 2. Store metadata in database
    /// 3. Return hash for announcement (caller sends via send_raw)
    pub async fn share_file(
        &self,
        file_path: &Path,
        topic_id: &[u8; 32],
        db: &Mutex<DbConnection>,
    ) -> Result<([u8; 32], u64, u32, u8, String), ShareError> {
        // Check file exists and is large enough
        let file_metadata = std::fs::metadata(file_path)
            .map_err(|_| ShareError::FileNotFound(file_path.display().to_string()))?;

        if file_metadata.len() < CHUNK_SIZE {
            return Err(ShareError::FileTooSmall(file_metadata.len()));
        }

        info!(
            file = %file_path.display(),
            size = file_metadata.len(),
            topic = hex::encode(&topic_id[..8]),
            "Starting file share"
        );

        // Import the file
        let (hash, size) = self.blob_store.import_file(file_path)?;

        let total_chunks = ((size + CHUNK_SIZE - 1) / CHUNK_SIZE) as u32;
        let num_sections = calculate_num_sections(total_chunks);

        let display_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unnamed")
            .to_string();

        // Store metadata in database
        {
            let db_lock = db.lock().await;
            insert_blob(
                &db_lock,
                &hash,
                topic_id,
                &self.our_id,
                &display_name,
                size,
                num_sections,
            )
            .map_err(|e| ShareError::Database(e.to_string()))?;

            // Mark as complete since we have the full file
            mark_blob_complete(&db_lock, &hash)
                .map_err(|e| ShareError::Database(e.to_string()))?;
        }

        info!(
            hash = hex::encode(&hash[..8]),
            size = size,
            chunks = total_chunks,
            sections = num_sections,
            "File imported for sharing"
        );

        Ok((hash, size, total_chunks, num_sections, display_name))
    }
}

// Note: pull_blob and push_section_to_peer are implemented in protocol/share.rs
// since they require transport (Protocol's request_chunks, connect_for_share, etc.)

/// Process an incoming share message - handles all business logic
///
/// This function:
/// 1. Routes the message to the appropriate handler
/// 2. Performs database operations
/// 3. Reads/writes chunks as needed
/// 4. Returns response messages to send back
///
/// # Arguments
/// * `message` - The incoming ShareMessage
/// * `sender_id` - The sender's EndpointID (from connection)
/// * `our_id` - Our EndpointID
/// * `db` - Database connection
/// * `blob_store` - Blob storage
///
/// # Returns
/// * Vec of response ShareMessages to send back (may be empty)
pub async fn process_incoming_share_message(
    message: ShareMessage,
    sender_id: [u8; 32],
    our_id: [u8; 32],
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
) -> Vec<ShareMessage> {
    match message {
        ShareMessage::ChunkMapRequest(req) => {
            handle_chunk_map_request(db, blob_store, our_id, req)
                .await
                .into_iter()
                .collect()
        }
        ShareMessage::ChunkRequest(req) => {
            handle_chunk_request(db, blob_store, sender_id, req).await
        }
        ShareMessage::Bitfield(bitfield) => {
            handle_bitfield(sender_id, bitfield);
            Vec::new()
        }
        ShareMessage::ChunkResponse(resp) => {
            handle_chunk_response(db, blob_store, sender_id, resp)
                .await
                .into_iter()
                .collect()
        }
        ShareMessage::PeerSuggestion(suggestion) => {
            debug!(
                hash = %hex::encode(&suggestion.hash[..8]),
                section = suggestion.section_id,
                suggested = %hex::encode(&suggestion.suggested_peer[..8]),
                "received peer suggestion"
            );
            Vec::new()
        }
        ShareMessage::ChunkAck(ack) => {
            trace!(
                hash = %hex::encode(&ack.hash[..8]),
                chunk = ack.chunk_index,
                "received chunk ack"
            );
            Vec::new()
        }
        ShareMessage::ChunkMapResponse(_) => {
            // Response message, shouldn't receive this as a request
            Vec::new()
        }
    }
}

/// Handle ChunkMapRequest - respond with who has what
async fn handle_chunk_map_request(
    db: &Mutex<DbConnection>,
    _blob_store: &BlobStore,
    our_id: [u8; 32],
    req: ChunkMapRequest,
) -> Option<ShareMessage> {
    let db_lock = db.lock().await;

    // Get blob metadata
    let blob = match get_blob(&db_lock, &req.hash) {
        Ok(Some(b)) => b,
        _ => return None,
    };

    // Get section traces
    let traces = match get_section_traces(&db_lock, &req.hash) {
        Ok(t) => t,
        Err(_) => return None,
    };

    let mut peers = Vec::new();

    // Add peers from traces
    for trace in traces {
        if let Some(peer_id) = trace.received_from {
            peers.push(PeerChunks {
                endpoint_id: peer_id,
                chunk_start: trace.chunk_start,
                chunk_end: trace.chunk_end,
            });
        }
    }

    // Add ourselves if we're the source and complete
    if blob.source_id == our_id && blob.state == BlobState::Complete {
        peers.push(PeerChunks {
            endpoint_id: our_id,
            chunk_start: 0,
            chunk_end: blob.total_chunks,
        });
    }

    Some(ShareMessage::ChunkMapResponse(ChunkMapResponse {
        hash: req.hash,
        peers,
    }))
}

/// Handle ChunkRequest - send requested chunks
///
/// Returns all requested chunks as multiple ChunkResponse messages
async fn handle_chunk_request(
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
    sender_id: [u8; 32],
    req: ChunkRequest,
) -> Vec<ShareMessage> {
    // Check if we have this blob
    {
        let db_lock = db.lock().await;
        match get_blob(&db_lock, &req.hash) {
            Ok(Some(_)) => {}
            _ => return Vec::new(),
        }
    };

    // Send all requested chunks
    let mut responses = Vec::with_capacity(req.chunks.len());

    for &chunk_index in &req.chunks {
        match blob_store.read_chunk(&req.hash, chunk_index) {
            Ok(data) => {
                trace!(
                    hash = %hex::encode(&req.hash[..8]),
                    chunk = chunk_index,
                    to = %hex::encode(&sender_id[..8]),
                    "sending chunk"
                );

                responses.push(ShareMessage::ChunkResponse(ChunkResponse {
                    hash: req.hash,
                    chunk_index,
                    data,
                }));
            }
            Err(e) => {
                debug!(error = %e, chunk = chunk_index, "failed to read chunk");
                // Continue with other chunks even if one fails
            }
        }
    }

    if !responses.is_empty() {
        debug!(
            hash = %hex::encode(&req.hash[..8]),
            chunks = responses.len(),
            to = %hex::encode(&sender_id[..8]),
            "sending {} chunks",
            responses.len()
        );
    }

    responses
}

/// Handle incoming Bitfield - log for tracking
fn handle_bitfield(sender_id: [u8; 32], bitfield: BitfieldMessage) {
    let chunk_count = bitfield.bitfield.len() * 8;
    trace!(
        hash = %hex::encode(&bitfield.hash[..8]),
        from = %hex::encode(&sender_id[..8]),
        chunks = chunk_count,
        "received bitfield"
    );
}

/// Handle incoming ChunkResponse - store the chunk
async fn handle_chunk_response(
    db: &Mutex<DbConnection>,
    blob_store: &BlobStore,
    sender_id: [u8; 32],
    resp: ChunkResponse,
) -> Option<ShareMessage> {
    // Get blob metadata for total_size
    let blob = {
        let db_lock = db.lock().await;
        match get_blob(&db_lock, &resp.hash) {
            Ok(Some(b)) => b,
            _ => return None,
        }
    };

    // Write chunk to storage
    if let Err(e) = blob_store.write_chunk(
        &resp.hash,
        resp.chunk_index,
        &resp.data,
        blob.total_size,
    ) {
        warn!(error = %e, "failed to write chunk");
        return None;
    }

    debug!(
        hash = %hex::encode(&resp.hash[..8]),
        chunk = resp.chunk_index,
        total = blob.total_chunks,
        from = %hex::encode(&sender_id[..8]),
        "SHARE: Received chunk"
    );

    // Check if blob is now complete
    if blob_store.is_complete(&resp.hash, blob.total_chunks).unwrap_or(false) {
        // Mark as complete in database
        {
            let db_lock = db.lock().await;
            if let Err(e) = mark_blob_complete(&db_lock, &resp.hash) {
                warn!(error = %e, "failed to mark blob complete");
            }
        }

        info!(
            hash = hex::encode(&resp.hash[..8]),
            name = blob.display_name,
            size = blob.total_size,
            chunks = blob.total_chunks,
            "SHARE: File complete"
        );
    }

    // Send acknowledgment
    Some(ShareMessage::ChunkAck(ChunkAck {
        hash: resp.hash,
        chunk_index: resp.chunk_index,
    }))
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

