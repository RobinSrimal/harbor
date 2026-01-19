//! CRDT Sync operations for the Protocol
//!
//! Provides SyncManager for collaborative document sync within topics.
//! Uses Loro CRDT for conflict-free merging.
//!
//! # Architecture
//!
//! - One SyncManager per topic
//! - One LoroDoc per topic (with multiple containers for different "documents")
//! - WAL persistence per operation (durability)
//! - Batched network sending (~1 second)
//! - Direct connection for initial sync (no size limit)

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use loro::LoroDoc;
use tracing::{debug, info, trace};

use crate::data::sync::SyncStore;
use crate::network::sync::protocol::{
    SyncMessage, SyncUpdate, InitialSyncRequest, InitialSyncResponse,
};
use crate::protocol::ProtocolError;

/// Default batch interval for network sending (1 second)
const DEFAULT_BATCH_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum responses to accept for initial sync
const MAX_INITIAL_SYNC_RESPONSES: usize = 3;

/// SyncManager - manages CRDT sync for a single topic
pub struct SyncManager {
    /// Topic ID
    topic_id: [u8; 32],
    /// Loro document
    doc: LoroDoc,
    /// Persistence store
    store: SyncStore,
    /// Version at last network broadcast
    last_broadcast_version: Vec<u8>,
    /// Whether there are pending changes to broadcast
    dirty: bool,
    /// Last flush time
    last_flush: Instant,
    /// Batch interval
    batch_interval: Duration,
    /// Pending initial sync requests (request_id -> responses received)
    pending_initial_syncs: HashMap<[u8; 32], usize>,
}

impl SyncManager {
    /// Create a new SyncManager for a topic
    ///
    /// Loads existing state from storage if available.
    pub fn new(sync_path: &PathBuf, topic_id: [u8; 32]) -> Result<Self, SyncError> {
        let mut store = SyncStore::new(sync_path, topic_id)
            .map_err(|e| SyncError::Storage(e.to_string()))?;
        
        let doc = LoroDoc::new();
        
        // Load existing state if any
        let loaded = store.load(&doc)
            .map_err(|e| SyncError::Storage(e.to_string()))?;
        
        if loaded {
            debug!(
                topic = %hex::encode(&topic_id[..8]),
                "Loaded existing sync document"
            );
        }
        
        let last_broadcast_version = doc.oplog_vv().encode();
        
        Ok(Self {
            topic_id,
            doc,
            store,
            last_broadcast_version,
            dirty: false,
            last_flush: Instant::now(),
            batch_interval: DEFAULT_BATCH_INTERVAL,
            pending_initial_syncs: HashMap::new(),
        })
    }
    
    /// Get the topic ID
    pub fn topic_id(&self) -> [u8; 32] {
        self.topic_id
    }
    
    /// Get access to the Loro document for making changes
    ///
    /// After making changes, call `on_local_change()` to persist and mark for broadcast.
    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }
    
    /// Get mutable access to the Loro document
    pub fn doc_mut(&mut self) -> &mut LoroDoc {
        &mut self.doc
    }
    
    /// Called after making local changes to the document
    ///
    /// This:
    /// 1. Persists the change to WAL immediately (durability)
    /// 2. Marks for batched network broadcast
    pub fn on_local_change(&mut self) -> Result<(), SyncError> {
        // Export delta since last known version
        let last_version = loro::VersionVector::decode(&self.last_broadcast_version)
            .map_err(|e| SyncError::Loro(e.to_string()))?;
        
        let delta = self.doc.export(loro::ExportMode::Updates { from: Cow::Borrowed(&last_version) })
            .map_err(|e| SyncError::Loro(e.to_string()))?;
        
        // Persist to WAL immediately (durability)
        self.store.append_to_wal(&delta)
            .map_err(|e| SyncError::Storage(e.to_string()))?;
        
        // Mark dirty for batched network send
        self.dirty = true;
        
        trace!(
            topic = %hex::encode(&self.topic_id[..8]),
            delta_size = delta.len(),
            "Local change persisted to WAL"
        );
        
        Ok(())
    }
    
    /// Check if there are pending changes to broadcast
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    
    /// Check if it's time to flush (batch interval elapsed)
    pub fn should_flush(&self) -> bool {
        self.dirty && self.last_flush.elapsed() >= self.batch_interval
    }
    
    /// Generate the batched update message for network broadcast
    ///
    /// Returns None if no changes to broadcast.
    pub fn generate_update(&mut self) -> Option<SyncMessage> {
        if !self.dirty {
            return None;
        }
        
        // Export all changes since last broadcast
        let last_version = loro::VersionVector::decode(&self.last_broadcast_version).ok()?;
        let delta = self.doc.export(loro::ExportMode::Updates { from: Cow::Borrowed(&last_version) }).ok()?;
        
        if delta.is_empty() {
            self.dirty = false;
            return None;
        }
        
        // Update tracking
        self.last_broadcast_version = self.doc.oplog_vv().encode();
        self.dirty = false;
        self.last_flush = Instant::now();
        
        debug!(
            topic = %hex::encode(&self.topic_id[..8]),
            delta_size = delta.len(),
            "Generated sync update for broadcast"
        );
        
        Some(SyncMessage::Update(SyncUpdate { data: delta }))
    }
    
    /// Apply an incoming update from another peer
    pub fn apply_update(&mut self, update: &SyncUpdate) -> Result<(), SyncError> {
        // Persist to WAL first (durability - ack after persist)
        self.store.append_to_wal(&update.data)
            .map_err(|e| SyncError::Storage(e.to_string()))?;
        
        // Apply to document
        self.doc.import(&update.data)
            .map_err(|e| SyncError::Loro(e.to_string()))?;
        
        trace!(
            topic = %hex::encode(&self.topic_id[..8]),
            update_size = update.data.len(),
            "Applied incoming sync update"
        );
        
        Ok(())
    }
    
    /// Generate an initial sync request
    pub fn generate_initial_sync_request(&mut self) -> SyncMessage {
        // Generate unique request ID
        let mut request_id = [0u8; 32];
        use rand::Rng;
        rand::thread_rng().fill(&mut request_id);
        
        // Track this request
        self.pending_initial_syncs.insert(request_id, 0);
        
        let version = self.doc.oplog_vv().encode();
        
        debug!(
            topic = %hex::encode(&self.topic_id[..8]),
            request_id = %hex::encode(&request_id[..8]),
            "Generated initial sync request"
        );
        
        SyncMessage::InitialSyncRequest(InitialSyncRequest {
            request_id,
            version,
        })
    }
    
    /// Handle an incoming initial sync request from another peer
    ///
    /// Returns the response to send via direct connection.
    pub fn handle_initial_sync_request(&self, request: &InitialSyncRequest) -> InitialSyncResponse {
        // Export full snapshot for the requester
        let snapshot = self.doc.export(loro::ExportMode::Snapshot)
            .unwrap_or_default();
        
        debug!(
            topic = %hex::encode(&self.topic_id[..8]),
            request_id = %hex::encode(&request.request_id[..8]),
            snapshot_size = snapshot.len(),
            "Generated initial sync response"
        );
        
        InitialSyncResponse {
            request_id: request.request_id,
            snapshot,
        }
    }
    
    /// Apply an initial sync response
    ///
    /// Returns true if this was a valid response we were waiting for.
    pub fn apply_initial_sync_response(&mut self, response: &InitialSyncResponse) -> Result<bool, SyncError> {
        // Check if we're expecting this response
        let count = self.pending_initial_syncs.get_mut(&response.request_id);
        
        match count {
            None => {
                // Not a response we requested
                return Ok(false);
            }
            Some(c) if *c >= MAX_INITIAL_SYNC_RESPONSES => {
                // Already received enough responses
                return Ok(false);
            }
            Some(c) => {
                *c += 1;
            }
        }
        
        // Persist to WAL first
        self.store.append_to_wal(&response.snapshot)
            .map_err(|e| SyncError::Storage(e.to_string()))?;
        
        // Apply snapshot (Loro handles merging)
        self.doc.import(&response.snapshot)
            .map_err(|e| SyncError::Loro(e.to_string()))?;
        
        // Update version tracking
        self.last_broadcast_version = self.doc.oplog_vv().encode();
        
        info!(
            topic = %hex::encode(&self.topic_id[..8]),
            request_id = %hex::encode(&response.request_id[..8]),
            snapshot_size = response.snapshot.len(),
            "Applied initial sync response"
        );
        
        Ok(true)
    }
    
    /// Save a periodic snapshot (compaction)
    pub fn save_snapshot(&mut self) -> Result<(), SyncError> {
        self.store.save_snapshot(&self.doc)
            .map_err(|e| SyncError::Storage(e.to_string()))
    }
    
    /// Delete all storage for this topic
    pub fn delete_storage(&mut self) -> Result<(), SyncError> {
        self.store.delete()
            .map_err(|e| SyncError::Storage(e.to_string()))
    }
    
    /// Set the batch interval for network sending
    pub fn set_batch_interval(&mut self, interval: Duration) {
        self.batch_interval = interval;
    }
}

/// Sync errors
#[derive(Debug)]
pub enum SyncError {
    Storage(String),
    Loro(String),
    Network(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Storage(e) => write!(f, "sync storage error: {}", e),
            SyncError::Loro(e) => write!(f, "Loro error: {}", e),
            SyncError::Network(e) => write!(f, "sync network error: {}", e),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<SyncError> for ProtocolError {
    fn from(e: SyncError) -> Self {
        ProtocolError::Sync(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }
    
    #[test]
    fn test_sync_manager_create() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SyncManager::new(&temp_dir.path().to_path_buf(), test_topic_id()).unwrap();
        
        assert_eq!(manager.topic_id(), test_topic_id());
        assert!(!manager.is_dirty());
    }
    
    #[test]
    fn test_local_changes_and_update() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = SyncManager::new(&temp_dir.path().to_path_buf(), test_topic_id()).unwrap();
        
        // Make a change
        manager.doc().get_text("content").insert(0, "Hello").unwrap();
        manager.on_local_change().unwrap();
        
        assert!(manager.is_dirty());
        
        // Generate update
        let update = manager.generate_update();
        assert!(update.is_some());
        assert!(!manager.is_dirty());
    }
    
    #[test]
    fn test_apply_update() {
        let temp_dir = TempDir::new().unwrap();
        
        // Manager 1 makes changes
        let mut manager1 = SyncManager::new(&temp_dir.path().to_path_buf(), test_topic_id()).unwrap();
        manager1.doc().get_text("content").insert(0, "Hello").unwrap();
        manager1.on_local_change().unwrap();
        
        let update = manager1.generate_update().unwrap();
        
        // Manager 2 receives update
        let temp_dir2 = TempDir::new().unwrap();
        let mut manager2 = SyncManager::new(&temp_dir2.path().to_path_buf(), test_topic_id()).unwrap();
        
        if let SyncMessage::Update(u) = update {
            manager2.apply_update(&u).unwrap();
        }
        
        // Should have same content
        assert_eq!(
            manager2.doc().get_text("content").to_string(),
            "Hello"
        );
    }
    
    #[test]
    fn test_initial_sync() {
        let temp_dir = TempDir::new().unwrap();
        
        // Manager 1 has content
        let mut manager1 = SyncManager::new(&temp_dir.path().to_path_buf(), test_topic_id()).unwrap();
        manager1.doc().get_text("content").insert(0, "Existing content").unwrap();
        manager1.on_local_change().unwrap();
        manager1.save_snapshot().unwrap();
        
        // Manager 2 is new, requests initial sync
        let temp_dir2 = TempDir::new().unwrap();
        let mut manager2 = SyncManager::new(&temp_dir2.path().to_path_buf(), test_topic_id()).unwrap();
        
        let request = manager2.generate_initial_sync_request();
        
        // Manager 1 responds
        if let SyncMessage::InitialSyncRequest(req) = request {
            let response = manager1.handle_initial_sync_request(&req);
            
            // Manager 2 applies response
            let applied = manager2.apply_initial_sync_response(&response).unwrap();
            assert!(applied);
            
            // Should have same content
            assert_eq!(
                manager2.doc().get_text("content").to_string(),
                "Existing content"
            );
        }
    }
    
    #[test]
    fn test_concurrent_edits_merge() {
        // Two managers making concurrent changes that get merged
        let temp_dir1 = TempDir::new().unwrap();
        let temp_dir2 = TempDir::new().unwrap();
        
        let mut manager1 = SyncManager::new(&temp_dir1.path().to_path_buf(), test_topic_id()).unwrap();
        let mut manager2 = SyncManager::new(&temp_dir2.path().to_path_buf(), test_topic_id()).unwrap();
        
        // Manager 1 edits "doc1"
        manager1.doc().get_text("doc1").insert(0, "From manager 1").unwrap();
        manager1.on_local_change().unwrap();
        
        // Manager 2 edits "doc2" concurrently
        manager2.doc().get_text("doc2").insert(0, "From manager 2").unwrap();
        manager2.on_local_change().unwrap();
        
        // Exchange updates
        let update1 = manager1.generate_update().unwrap();
        let update2 = manager2.generate_update().unwrap();
        
        if let SyncMessage::Update(u1) = update1 {
            manager2.apply_update(&u1).unwrap();
        }
        if let SyncMessage::Update(u2) = update2 {
            manager1.apply_update(&u2).unwrap();
        }
        
        // Both should have both documents
        assert_eq!(manager1.doc().get_text("doc1").to_string(), "From manager 1");
        assert_eq!(manager1.doc().get_text("doc2").to_string(), "From manager 2");
        assert_eq!(manager2.doc().get_text("doc1").to_string(), "From manager 1");
        assert_eq!(manager2.doc().get_text("doc2").to_string(), "From manager 2");
    }
    
    #[test]
    fn test_multiple_initial_sync_responses_limit() {
        let temp_dir1 = TempDir::new().unwrap();
        let temp_dir2 = TempDir::new().unwrap();
        
        // Manager 1 has content
        let mut manager1 = SyncManager::new(&temp_dir1.path().to_path_buf(), test_topic_id()).unwrap();
        manager1.doc().get_text("content").insert(0, "Hello").unwrap();
        manager1.on_local_change().unwrap();
        
        // Manager 2 requests initial sync
        let mut manager2 = SyncManager::new(&temp_dir2.path().to_path_buf(), test_topic_id()).unwrap();
        let request = manager2.generate_initial_sync_request();
        
        if let SyncMessage::InitialSyncRequest(req) = request {
            let response = manager1.handle_initial_sync_request(&req);
            
            // First 3 responses should be accepted
            assert!(manager2.apply_initial_sync_response(&response).unwrap());
            assert!(manager2.apply_initial_sync_response(&response).unwrap());
            assert!(manager2.apply_initial_sync_response(&response).unwrap());
            
            // 4th response should be rejected (limit is 3)
            assert!(!manager2.apply_initial_sync_response(&response).unwrap());
        }
    }
    
    #[test]
    fn test_batch_interval_timing() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = SyncManager::new(&temp_dir.path().to_path_buf(), test_topic_id()).unwrap();
        
        // Set very short batch interval for testing
        manager.set_batch_interval(Duration::from_millis(10));
        
        // Make a change
        manager.doc().get_text("test").insert(0, "data").unwrap();
        manager.on_local_change().unwrap();
        
        // Should not flush immediately
        assert!(manager.is_dirty());
        assert!(!manager.should_flush()); // Interval hasn't elapsed
        
        // Wait for interval
        std::thread::sleep(Duration::from_millis(15));
        
        // Now should be ready to flush
        assert!(manager.should_flush());
        
        // After generating update, should not be dirty
        let _update = manager.generate_update();
        assert!(!manager.is_dirty());
        assert!(!manager.should_flush());
    }
    
    #[test]
    fn test_unrequested_response_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let mut manager = SyncManager::new(&temp_dir.path().to_path_buf(), test_topic_id()).unwrap();
        
        // Create a response for a request we never made
        let fake_response = InitialSyncResponse {
            request_id: [99u8; 32], // Random ID we didn't request
            snapshot: vec![1, 2, 3],
        };
        
        // Should be rejected
        let applied = manager.apply_initial_sync_response(&fake_response).unwrap();
        assert!(!applied);
    }
    
    #[test]
    fn test_persistence_across_restart() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().to_path_buf();
        
        // Create manager 1, make changes, save snapshot
        {
            let mut manager = SyncManager::new(&path, test_topic_id()).unwrap();
            manager.doc().get_text("persistent").insert(0, "Survived restart").unwrap();
            manager.on_local_change().unwrap();
            manager.save_snapshot().unwrap();
        }
        
        // Create manager 2 with same path - should load existing state
        {
            let manager = SyncManager::new(&path, test_topic_id()).unwrap();
            assert_eq!(
                manager.doc().get_text("persistent").to_string(),
                "Survived restart"
            );
        }
    }
    
    #[test]
    fn test_delete_storage() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().to_path_buf();
        
        // Create and save
        let mut manager = SyncManager::new(&path, test_topic_id()).unwrap();
        manager.doc().get_text("test").insert(0, "data").unwrap();
        manager.on_local_change().unwrap();
        manager.save_snapshot().unwrap();
        
        // Delete
        manager.delete_storage().unwrap();
        
        // New manager should start fresh
        let manager2 = SyncManager::new(&path, test_topic_id()).unwrap();
        assert_eq!(manager2.doc().get_text("test").to_string(), "");
    }
}

