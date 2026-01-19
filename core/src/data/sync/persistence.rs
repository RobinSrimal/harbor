//! Sync persistence - WAL + Snapshots
//!
//! Two-tier persistence for durability and efficiency:
//! - WAL: Per-operation writes (immediate, durable)
//! - Snapshot: Periodic full state (for fast recovery)

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use loro::LoroDoc;
use tracing::{debug, info, warn};

/// Default sync storage path
pub fn default_sync_path() -> Result<PathBuf, std::io::Error> {
    let path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".harbor_sync");
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Sync store for a single topic
pub struct SyncStore {
    /// Topic ID
    topic_id: [u8; 32],
    /// Base directory for sync files
    #[allow(dead_code)]
    base_path: PathBuf,
    /// Snapshot file path
    snapshot_path: PathBuf,
    /// WAL file path
    wal_path: PathBuf,
    /// WAL file handle (kept open for appending)
    wal_file: Option<File>,
}

impl SyncStore {
    /// Create a new sync store for a topic
    pub fn new(base_path: &Path, topic_id: [u8; 32]) -> Result<Self, SyncStoreError> {
        let topic_hex = hex::encode(topic_id);
        let snapshot_path = base_path.join(format!("{}.loro", topic_hex));
        let wal_path = base_path.join(format!("{}.wal", topic_hex));
        
        // Ensure base directory exists
        fs::create_dir_all(base_path)
            .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        
        Ok(Self {
            topic_id,
            base_path: base_path.to_path_buf(),
            snapshot_path,
            wal_path,
            wal_file: None,
        })
    }
    
    /// Open the WAL file for appending
    fn open_wal(&mut self) -> Result<&mut File, SyncStoreError> {
        if self.wal_file.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.wal_path)
                .map_err(|e| SyncStoreError::Io(e.to_string()))?;
            self.wal_file = Some(file);
        }
        Ok(self.wal_file.as_mut().unwrap())
    }
    
    /// Append a delta to the WAL (per-operation, immediate)
    /// 
    /// Format: [4-byte length][delta bytes]
    pub fn append_to_wal(&mut self, delta: &[u8]) -> Result<(), SyncStoreError> {
        let file = self.open_wal()?;
        
        // Write length prefix
        let len = delta.len() as u32;
        file.write_all(&len.to_le_bytes())
            .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        
        // Write delta
        file.write_all(delta)
            .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        
        // Flush to disk immediately (durability)
        file.sync_data()
            .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        
        Ok(())
    }
    
    /// Save a snapshot (periodic, replaces WAL)
    pub fn save_snapshot(&mut self, doc: &LoroDoc) -> Result<(), SyncStoreError> {
        // Export shallow snapshot (compact, no old history)
        let snapshot = doc.export(loro::ExportMode::Snapshot)
            .map_err(|e| SyncStoreError::Loro(e.to_string()))?;
        
        // Write to temp file first, then rename (atomic)
        let temp_path = self.snapshot_path.with_extension("loro.tmp");
        fs::write(&temp_path, &snapshot)
            .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        fs::rename(&temp_path, &self.snapshot_path)
            .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        
        // Clear WAL (all changes now in snapshot)
        self.clear_wal()?;
        
        debug!(
            topic = %hex::encode(&self.topic_id[..8]),
            snapshot_size = snapshot.len(),
            "Saved sync snapshot"
        );
        
        Ok(())
    }
    
    /// Clear the WAL
    fn clear_wal(&mut self) -> Result<(), SyncStoreError> {
        // Close existing file handle
        self.wal_file = None;
        
        // Truncate WAL file
        if self.wal_path.exists() {
            fs::write(&self.wal_path, [])
                .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        }
        
        Ok(())
    }
    
    /// Load document from storage (snapshot + WAL replay)
    pub fn load(&mut self, doc: &LoroDoc) -> Result<bool, SyncStoreError> {
        let mut loaded_anything = false;
        
        // 1. Load snapshot if exists
        if self.snapshot_path.exists() {
            let snapshot = fs::read(&self.snapshot_path)
                .map_err(|e| SyncStoreError::Io(e.to_string()))?;
            
            if !snapshot.is_empty() {
                doc.import(&snapshot)
                    .map_err(|e| SyncStoreError::Loro(e.to_string()))?;
                loaded_anything = true;
                debug!(
                    topic = %hex::encode(&self.topic_id[..8]),
                    snapshot_size = snapshot.len(),
                    "Loaded sync snapshot"
                );
            }
        }
        
        // 2. Replay WAL if exists
        if self.wal_path.exists() {
            let wal_data = fs::read(&self.wal_path)
                .map_err(|e| SyncStoreError::Io(e.to_string()))?;
            
            if !wal_data.is_empty() {
                let ops_count = self.replay_wal(doc, &wal_data)?;
                if ops_count > 0 {
                    loaded_anything = true;
                    debug!(
                        topic = %hex::encode(&self.topic_id[..8]),
                        ops_count = ops_count,
                        "Replayed WAL entries"
                    );
                }
            }
        }
        
        // 3. Compact after recovery (save fresh snapshot, clear WAL)
        if loaded_anything {
            self.save_snapshot(doc)?;
        }
        
        Ok(loaded_anything)
    }
    
    /// Replay WAL entries into document
    fn replay_wal(&self, doc: &LoroDoc, wal_data: &[u8]) -> Result<usize, SyncStoreError> {
        let mut cursor = 0;
        let mut count = 0;
        
        while cursor + 4 <= wal_data.len() {
            // Read length
            let len = u32::from_le_bytes([
                wal_data[cursor],
                wal_data[cursor + 1],
                wal_data[cursor + 2],
                wal_data[cursor + 3],
            ]) as usize;
            cursor += 4;
            
            // Check bounds
            if cursor + len > wal_data.len() {
                warn!(
                    topic = %hex::encode(&self.topic_id[..8]),
                    "WAL truncated, stopping replay"
                );
                break;
            }
            
            // Read and apply delta
            let delta = &wal_data[cursor..cursor + len];
            
            // Loro handles duplicates gracefully (idempotent)
            if let Err(e) = doc.import(delta) {
                warn!(
                    topic = %hex::encode(&self.topic_id[..8]),
                    error = %e,
                    "Failed to apply WAL entry, skipping"
                );
            } else {
                count += 1;
            }
            
            cursor += len;
        }
        
        Ok(count)
    }
    
    /// Delete all storage for this topic
    pub fn delete(&mut self) -> Result<(), SyncStoreError> {
        self.wal_file = None;
        
        if self.snapshot_path.exists() {
            fs::remove_file(&self.snapshot_path)
                .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        }
        
        if self.wal_path.exists() {
            fs::remove_file(&self.wal_path)
                .map_err(|e| SyncStoreError::Io(e.to_string()))?;
        }
        
        info!(
            topic = %hex::encode(&self.topic_id[..8]),
            "Deleted sync storage"
        );
        
        Ok(())
    }
    
    /// Check if any storage exists for this topic
    pub fn exists(&self) -> bool {
        self.snapshot_path.exists() || self.wal_path.exists()
    }
    
    /// Get the topic ID
    pub fn topic_id(&self) -> [u8; 32] {
        self.topic_id
    }
}

/// Sync store errors
#[derive(Debug)]
pub enum SyncStoreError {
    Io(String),
    Loro(String),
}

impl std::fmt::Display for SyncStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncStoreError::Io(e) => write!(f, "IO error: {}", e),
            SyncStoreError::Loro(e) => write!(f, "Loro error: {}", e),
        }
    }
}

impl std::error::Error for SyncStoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }
    
    #[test]
    fn test_wal_append_and_replay() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        
        // Create a doc and make some changes
        let doc1 = LoroDoc::new();
        let text = doc1.get_text("content");
        text.insert(0, "Hello").unwrap();
        
        // Export delta and append to WAL
        let delta = doc1.export(loro::ExportMode::Snapshot).unwrap();
        store.append_to_wal(&delta).unwrap();
        
        // Create new doc and load from storage
        let doc2 = LoroDoc::new();
        store.load(&doc2).unwrap();
        
        // Should have same content
        let text2 = doc2.get_text("content");
        assert_eq!(text2.to_string(), "Hello");
    }
    
    #[test]
    fn test_snapshot_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        
        // Create a doc with content
        let doc1 = LoroDoc::new();
        let text = doc1.get_text("content");
        text.insert(0, "Hello World").unwrap();
        
        // Save snapshot
        store.save_snapshot(&doc1).unwrap();
        
        // Create new store (simulating restart)
        let mut store2 = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        let doc2 = LoroDoc::new();
        store2.load(&doc2).unwrap();
        
        // Should have same content
        let text2 = doc2.get_text("content");
        assert_eq!(text2.to_string(), "Hello World");
    }
    
    #[test]
    fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        
        // Create content
        let doc = LoroDoc::new();
        doc.get_text("test").insert(0, "data").unwrap();
        store.save_snapshot(&doc).unwrap();
        
        assert!(store.exists());
        
        // Delete
        store.delete().unwrap();
        
        assert!(!store.exists());
    }
    
    #[test]
    fn test_wal_crash_recovery() {
        let temp_dir = TempDir::new().unwrap();
        
        // Simulate: write to WAL, then "crash" before snapshot
        {
            let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
            
            let doc = LoroDoc::new();
            doc.get_text("content").insert(0, "Before crash").unwrap();
            
            // Only write to WAL, don't save snapshot
            let delta = doc.export(loro::ExportMode::Snapshot).unwrap();
            store.append_to_wal(&delta).unwrap();
            
            // "Crash" - drop store without snapshot
        }
        
        // Recover: new store should replay WAL
        {
            let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
            let doc = LoroDoc::new();
            
            let recovered = store.load(&doc).unwrap();
            assert!(recovered);
            
            assert_eq!(doc.get_text("content").to_string(), "Before crash");
        }
    }
    
    #[test]
    fn test_wal_multiple_entries() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        
        // Write multiple entries to WAL
        let doc = LoroDoc::new();
        
        doc.get_text("content").insert(0, "First").unwrap();
        let delta1 = doc.export(loro::ExportMode::Snapshot).unwrap();
        store.append_to_wal(&delta1).unwrap();
        
        doc.get_text("content").insert(5, " Second").unwrap();
        let delta2 = doc.export(loro::ExportMode::Snapshot).unwrap();
        store.append_to_wal(&delta2).unwrap();
        
        doc.get_text("content").insert(12, " Third").unwrap();
        let delta3 = doc.export(loro::ExportMode::Snapshot).unwrap();
        store.append_to_wal(&delta3).unwrap();
        
        // New store should recover all entries
        let mut store2 = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        let doc2 = LoroDoc::new();
        store2.load(&doc2).unwrap();
        
        assert_eq!(doc2.get_text("content").to_string(), "First Second Third");
    }
    
    #[test]
    fn test_snapshot_clears_wal() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        
        let doc = LoroDoc::new();
        doc.get_text("test").insert(0, "data").unwrap();
        
        // Write to WAL
        let delta = doc.export(loro::ExportMode::Snapshot).unwrap();
        store.append_to_wal(&delta).unwrap();
        
        // WAL file should exist and have content
        let wal_path = temp_dir.path().join(format!("{}.wal", hex::encode(test_topic_id())));
        assert!(wal_path.exists());
        assert!(std::fs::metadata(&wal_path).unwrap().len() > 0);
        
        // Save snapshot - should clear WAL
        store.save_snapshot(&doc).unwrap();
        
        // WAL should be empty now
        assert_eq!(std::fs::metadata(&wal_path).unwrap().len(), 0);
    }
    
    #[test]
    fn test_empty_store_load() {
        let temp_dir = TempDir::new().unwrap();
        let mut store = SyncStore::new(temp_dir.path(), test_topic_id()).unwrap();
        
        let doc = LoroDoc::new();
        let loaded = store.load(&doc).unwrap();
        
        // Should return false for empty store
        assert!(!loaded);
        
        // Document should be empty
        assert_eq!(doc.get_text("anything").to_string(), "");
    }
    
    #[test]
    fn test_topic_id_isolation() {
        let temp_dir = TempDir::new().unwrap();
        
        let topic1 = [1u8; 32];
        let topic2 = [2u8; 32];
        
        // Write to topic 1
        {
            let mut store = SyncStore::new(temp_dir.path(), topic1).unwrap();
            let doc = LoroDoc::new();
            doc.get_text("content").insert(0, "Topic 1 data").unwrap();
            store.save_snapshot(&doc).unwrap();
        }
        
        // Write to topic 2
        {
            let mut store = SyncStore::new(temp_dir.path(), topic2).unwrap();
            let doc = LoroDoc::new();
            doc.get_text("content").insert(0, "Topic 2 data").unwrap();
            store.save_snapshot(&doc).unwrap();
        }
        
        // Load topic 1 - should only have topic 1 data
        {
            let mut store = SyncStore::new(temp_dir.path(), topic1).unwrap();
            let doc = LoroDoc::new();
            store.load(&doc).unwrap();
            assert_eq!(doc.get_text("content").to_string(), "Topic 1 data");
        }
        
        // Load topic 2 - should only have topic 2 data
        {
            let mut store = SyncStore::new(temp_dir.path(), topic2).unwrap();
            let doc = LoroDoc::new();
            store.load(&doc).unwrap();
            assert_eq!(doc.get_text("content").to_string(), "Topic 2 data");
        }
    }
}

