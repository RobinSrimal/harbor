//! Share protocol database operations
//!
//! Handles blob metadata storage and retrieval for the file sharing protocol.
//! Files ≥512 KB are tracked here with their distribution status.

use rusqlite::{Connection, params};

use crate::data::dht::current_timestamp;

/// Chunk size for file sharing (512 KB)
pub const CHUNK_SIZE: u64 = 512 * 1024;

/// Blob state
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobState {
    /// Partial - still downloading or distributing
    Partial = 0,
    /// Complete - all chunks present
    Complete = 1,
}

impl From<u8> for BlobState {
    fn from(v: u8) -> Self {
        match v {
            1 => BlobState::Complete,
            _ => BlobState::Partial,
        }
    }
}

/// Blob metadata from database
#[derive(Debug, Clone)]
pub struct BlobMetadata {
    pub hash: [u8; 32],
    pub scope_id: [u8; 32],
    pub source_id: [u8; 32],
    pub display_name: String,
    pub total_size: u64,
    pub total_chunks: u32,
    pub num_sections: u8,
    pub state: BlobState,
    pub created_at: i64,
}

/// Section trace - who sent us a section
#[derive(Debug, Clone)]
pub struct SectionTrace {
    pub section_id: u8,
    pub chunk_start: u32,
    pub chunk_end: u32,
    pub received_from: Option<[u8; 32]>,
    pub received_at: Option<i64>,
}

/// Insert a new blob record
pub fn insert_blob(
    conn: &Connection,
    hash: &[u8; 32],
    scope_id: &[u8; 32],
    source_id: &[u8; 32],
    display_name: &str,
    total_size: u64,
    num_sections: u8,
) -> rusqlite::Result<()> {
    let total_chunks = total_size.div_ceil(CHUNK_SIZE) as u32;

    conn.execute(
        "INSERT OR REPLACE INTO blobs
         (hash, scope_id, source_id, display_name, total_size, total_chunks, num_sections, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
        params![
            hash.as_slice(),
            scope_id.as_slice(),
            source_id.as_slice(),
            display_name,
            total_size as i64,
            total_chunks as i64,
            num_sections as i64,
        ],
    )?;
    Ok(())
}

/// Get blob metadata by hash
pub fn get_blob(conn: &Connection, hash: &[u8; 32]) -> rusqlite::Result<Option<BlobMetadata>> {
    let mut stmt = conn.prepare(
        "SELECT hash, scope_id, source_id, display_name, total_size, total_chunks,
                num_sections, state, created_at
         FROM blobs WHERE hash = ?1"
    )?;

    let result = stmt.query_row([hash.as_slice()], |row| {
        let hash_vec: Vec<u8> = row.get(0)?;
        let scope_vec: Vec<u8> = row.get(1)?;
        let source_vec: Vec<u8> = row.get(2)?;

        let mut hash = [0u8; 32];
        let mut scope_id = [0u8; 32];
        let mut source_id = [0u8; 32];
        hash.copy_from_slice(&hash_vec);
        scope_id.copy_from_slice(&scope_vec);
        source_id.copy_from_slice(&source_vec);

        Ok(BlobMetadata {
            hash,
            scope_id,
            source_id,
            display_name: row.get(3)?,
            total_size: row.get::<_, i64>(4)? as u64,
            total_chunks: row.get::<_, i64>(5)? as u32,
            num_sections: row.get::<_, i64>(6)? as u8,
            state: BlobState::from(row.get::<_, i64>(7)? as u8),
            created_at: row.get(8)?,
        })
    });
    
    match result {
        Ok(meta) => Ok(Some(meta)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Mark a blob as complete
pub fn mark_blob_complete(conn: &Connection, hash: &[u8; 32]) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE blobs SET state = 1 WHERE hash = ?1",
        [hash.as_slice()],
    )?;
    Ok(())
}

/// Initialize sections for a blob (called when receiving FileAnnouncement)
pub fn init_blob_sections(
    conn: &Connection,
    hash: &[u8; 32],
    num_sections: u8,
    total_chunks: u32,
) -> rusqlite::Result<()> {
    let chunks_per_section = total_chunks.div_ceil(num_sections as u32);
    
    for section_id in 0..num_sections {
        let chunk_start = section_id as u32 * chunks_per_section;
        let chunk_end = ((section_id as u32 + 1) * chunks_per_section).min(total_chunks);
        
        conn.execute(
            "INSERT OR IGNORE INTO blob_sections 
             (hash, section_id, chunk_start, chunk_end)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                hash.as_slice(),
                section_id as i64,
                chunk_start as i64,
                chunk_end as i64,
            ],
        )?;
    }
    Ok(())
}

/// Record that we received a section from a peer (for trace/suggestions)
pub fn record_section_received(
    conn: &Connection,
    hash: &[u8; 32],
    section_id: u8,
    received_from: &[u8; 32],
) -> rusqlite::Result<()> {
    let now = current_timestamp();
    conn.execute(
        "UPDATE blob_sections 
         SET received_from = ?1, received_at = ?2
         WHERE hash = ?3 AND section_id = ?4",
        params![
            received_from.as_slice(),
            now,
            hash.as_slice(),
            section_id as i64,
        ],
    )?;
    Ok(())
}

/// Get section traces for a blob (for suggesting peers)
pub fn get_section_traces(
    conn: &Connection,
    hash: &[u8; 32],
) -> rusqlite::Result<Vec<SectionTrace>> {
    let mut stmt = conn.prepare(
        "SELECT section_id, chunk_start, chunk_end, received_from, received_at
         FROM blob_sections WHERE hash = ?1 ORDER BY section_id"
    )?;
    
    let rows = stmt.query_map([hash.as_slice()], |row| {
        let received_from: Option<Vec<u8>> = row.get(3)?;
        let received_from = received_from.map(|v| {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&v);
            arr
        });
        
        Ok(SectionTrace {
            section_id: row.get::<_, i64>(0)? as u8,
            chunk_start: row.get::<_, i64>(1)? as u32,
            chunk_end: row.get::<_, i64>(2)? as u32,
            received_from,
            received_at: row.get(4)?,
        })
    })?;
    
    rows.collect()
}

/// Get peer suggestion for a section (from trace)
pub fn get_section_peer_suggestion(
    conn: &Connection,
    hash: &[u8; 32],
    section_id: u8,
) -> rusqlite::Result<Option<[u8; 32]>> {
    let result: rusqlite::Result<Vec<u8>> = conn.query_row(
        "SELECT received_from FROM blob_sections 
         WHERE hash = ?1 AND section_id = ?2 AND received_from IS NOT NULL",
        params![hash.as_slice(), section_id as i64],
        |row| row.get(0),
    );
    
    match result {
        Ok(v) => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&v);
            Ok(Some(arr))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Add a recipient for a blob (for tracking distribution)
pub fn add_blob_recipient(
    conn: &Connection,
    hash: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO blob_recipients (hash, endpoint_id, received)
         VALUES (?1, ?2, 0)",
        params![hash.as_slice(), endpoint_id.as_slice()],
    )?;
    Ok(())
}

/// Mark a recipient as having received the blob (CanSeed received)
pub fn mark_recipient_complete(
    conn: &Connection,
    hash: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<()> {
    let now = current_timestamp();
    conn.execute(
        "UPDATE blob_recipients SET received = 1, received_at = ?1
         WHERE hash = ?2 AND endpoint_id = ?3",
        params![now, hash.as_slice(), endpoint_id.as_slice()],
    )?;
    Ok(())
}

/// Check if all recipients have received the blob
pub fn is_distribution_complete(conn: &Connection, hash: &[u8; 32]) -> rusqlite::Result<bool> {
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blob_recipients 
         WHERE hash = ?1 AND received = 0",
        [hash.as_slice()],
        |row| row.get(0),
    )?;
    Ok(pending == 0)
}

/// Record that a peer can seed (has all sections of a blob)
/// This is called when receiving a CanSeed announcement
pub fn record_peer_can_seed(
    conn: &Connection,
    hash: &[u8; 32],
    peer_id: &[u8; 32],
) -> rusqlite::Result<()> {
    // Get the blob metadata to know how many sections
    let num_sections: i64 = match conn.query_row(
        "SELECT num_sections FROM blobs WHERE hash = ?1",
        [hash.as_slice()],
        |row| row.get(0),
    ) {
        Ok(n) => n,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()), // Blob not known, ignore
        Err(e) => return Err(e),
    };
    
    // Record this peer as having all sections
    let now = current_timestamp();
    for section_id in 0..num_sections as u8 {
        // Use INSERT OR REPLACE to upsert the trace
        conn.execute(
            "INSERT OR REPLACE INTO blob_sections 
             (hash, section_id, chunk_start, chunk_end, received_from, received_at)
             SELECT hash, section_id, chunk_start, chunk_end, ?1, ?2
             FROM blob_sections WHERE hash = ?3 AND section_id = ?4",
            params![
                peer_id.as_slice(),
                now,
                hash.as_slice(),
                section_id as i64,
            ],
        )?;
    }
    
    // Also add them as a recipient and mark complete
    add_blob_recipient(conn, hash, peer_id)?;
    mark_recipient_complete(conn, hash, peer_id)?;
    
    Ok(())
}

/// Get blobs for a scope (topic_id or endpoint_id)
pub fn get_blobs_for_scope(conn: &Connection, scope_id: &[u8; 32]) -> rusqlite::Result<Vec<BlobMetadata>> {
    let mut stmt = conn.prepare(
        "SELECT hash, scope_id, source_id, display_name, total_size, total_chunks,
                num_sections, state, created_at
         FROM blobs WHERE scope_id = ?1"
    )?;

    let rows = stmt.query_map([scope_id.as_slice()], |row| {
        let hash_vec: Vec<u8> = row.get(0)?;
        let scope_vec: Vec<u8> = row.get(1)?;
        let source_vec: Vec<u8> = row.get(2)?;

        let mut hash = [0u8; 32];
        let mut scope_id = [0u8; 32];
        let mut source_id = [0u8; 32];
        hash.copy_from_slice(&hash_vec);
        scope_id.copy_from_slice(&scope_vec);
        source_id.copy_from_slice(&source_vec);

        Ok(BlobMetadata {
            hash,
            scope_id,
            source_id,
            display_name: row.get(3)?,
            total_size: row.get::<_, i64>(4)? as u64,
            total_chunks: row.get::<_, i64>(5)? as u32,
            num_sections: row.get::<_, i64>(6)? as u8,
            state: BlobState::from(row.get::<_, i64>(7)? as u8),
            created_at: row.get(8)?,
        })
    })?;
    
    rows.collect()
}

/// Delete a blob and its associated data
pub fn delete_blob(conn: &Connection, hash: &[u8; 32]) -> rusqlite::Result<()> {
    // CASCADE will handle blob_recipients and blob_sections
    conn.execute("DELETE FROM blobs WHERE hash = ?1", [hash.as_slice()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_and_get_blob() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 1024 * 1024, 3).unwrap();
        
        let blob = get_blob(&conn, &hash).unwrap().unwrap();
        assert_eq!(blob.hash, hash);
        assert_eq!(blob.display_name, "test.bin");
        assert_eq!(blob.total_size, 1024 * 1024);
        assert_eq!(blob.total_chunks, 2); // 1MB / 512KB = 2 chunks
        assert_eq!(blob.num_sections, 3);
        assert_eq!(blob.state, BlobState::Partial);
    }

    #[test]
    fn test_section_traces() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        let peer_id = [5u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 5 * 1024 * 1024, 3).unwrap();
        init_blob_sections(&conn, &hash, 3, 10).unwrap();
        
        // Record receiving section 1 from peer
        record_section_received(&conn, &hash, 1, &peer_id).unwrap();
        
        // Get suggestion
        let suggestion = get_section_peer_suggestion(&conn, &hash, 1).unwrap();
        assert_eq!(suggestion, Some(peer_id));
        
        // No suggestion for section 0
        let no_suggestion = get_section_peer_suggestion(&conn, &hash, 0).unwrap();
        assert_eq!(no_suggestion, None);
    }

    #[test]
    fn test_recipient_tracking() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        let peer1 = [5u8; 32];
        let peer2 = [6u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 1024 * 1024, 3).unwrap();
        
        add_blob_recipient(&conn, &hash, &peer1).unwrap();
        add_blob_recipient(&conn, &hash, &peer2).unwrap();
        
        // Not complete yet
        assert!(!is_distribution_complete(&conn, &hash).unwrap());
        
        // Mark peer1 complete
        mark_recipient_complete(&conn, &hash, &peer1).unwrap();
        assert!(!is_distribution_complete(&conn, &hash).unwrap());
        
        // Mark peer2 complete
        mark_recipient_complete(&conn, &hash, &peer2).unwrap();
        assert!(is_distribution_complete(&conn, &hash).unwrap());
    }

    #[test]
    fn test_mark_blob_complete() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 1024 * 1024, 3).unwrap();
        
        // Initially partial
        let blob = get_blob(&conn, &hash).unwrap().unwrap();
        assert_eq!(blob.state, BlobState::Partial);
        
        // Mark complete
        mark_blob_complete(&conn, &hash).unwrap();
        
        let blob = get_blob(&conn, &hash).unwrap().unwrap();
        assert_eq!(blob.state, BlobState::Complete);
    }

    #[test]
    fn test_get_blobs_for_scope() {
        let conn = setup_db();
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];

        // Insert multiple blobs
        let hash1 = [10u8; 32];
        let hash2 = [11u8; 32];
        let hash3 = [12u8; 32];

        insert_blob(&conn, &hash1, &scope_id, &source_id, "file1.bin", 1024 * 1024, 3).unwrap();
        insert_blob(&conn, &hash2, &scope_id, &source_id, "file2.bin", 2 * 1024 * 1024, 3).unwrap();
        insert_blob(&conn, &hash3, &scope_id, &source_id, "file3.bin", 3 * 1024 * 1024, 3).unwrap();

        let blobs = get_blobs_for_scope(&conn, &scope_id).unwrap();
        assert_eq!(blobs.len(), 3);
        
        let names: Vec<_> = blobs.iter().map(|b| b.display_name.as_str()).collect();
        assert!(names.contains(&"file1.bin"));
        assert!(names.contains(&"file2.bin"));
        assert!(names.contains(&"file3.bin"));
    }

    #[test]
    fn test_delete_blob_cascades() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        let peer1 = [5u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 5 * 1024 * 1024, 3).unwrap();
        init_blob_sections(&conn, &hash, 3, 10).unwrap();
        add_blob_recipient(&conn, &hash, &peer1).unwrap();
        record_section_received(&conn, &hash, 0, &peer1).unwrap();
        
        // Verify data exists
        assert!(get_blob(&conn, &hash).unwrap().is_some());
        
        // Delete
        delete_blob(&conn, &hash).unwrap();
        
        // Verify cascade
        assert!(get_blob(&conn, &hash).unwrap().is_none());
        assert!(get_section_peer_suggestion(&conn, &hash, 0).unwrap().is_none());
    }

    #[test]
    fn test_get_section_traces() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        let peer1 = [5u8; 32];
        let peer2 = [6u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 5 * 1024 * 1024, 3).unwrap();
        init_blob_sections(&conn, &hash, 3, 10).unwrap();
        
        // Record different peers for different sections
        // (each section can only have one received_from - UPDATE overwrites)
        record_section_received(&conn, &hash, 0, &peer1).unwrap();
        record_section_received(&conn, &hash, 1, &peer2).unwrap();
        record_section_received(&conn, &hash, 2, &peer1).unwrap();
        
        // Get all traces - only sections with received_from set
        let traces = get_section_traces(&conn, &hash).unwrap();
        assert_eq!(traces.len(), 3); // 3 sections with traces
        
        // Verify each section has correct peer
        let section0 = traces.iter().find(|t| t.section_id == 0).unwrap();
        let section1 = traces.iter().find(|t| t.section_id == 1).unwrap();
        let section2 = traces.iter().find(|t| t.section_id == 2).unwrap();
        
        assert_eq!(section0.received_from, Some(peer1));
        assert_eq!(section1.received_from, Some(peer2));
        assert_eq!(section2.received_from, Some(peer1));
    }

    #[test]
    fn test_get_blob_nonexistent() {
        let conn = setup_db();
        let hash = [99u8; 32];
        
        let result = get_blob(&conn, &hash).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_multiple_sections_different_peers() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        let peer1 = [5u8; 32];
        let peer2 = [6u8; 32];
        let peer3 = [7u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 10 * 1024 * 1024, 5).unwrap();
        init_blob_sections(&conn, &hash, 5, 20).unwrap();
        
        // Each peer gets different sections
        record_section_received(&conn, &hash, 0, &peer1).unwrap();
        record_section_received(&conn, &hash, 1, &peer1).unwrap();
        record_section_received(&conn, &hash, 2, &peer2).unwrap();
        record_section_received(&conn, &hash, 3, &peer2).unwrap();
        record_section_received(&conn, &hash, 4, &peer3).unwrap();
        
        // Get suggestions for each section
        assert_eq!(get_section_peer_suggestion(&conn, &hash, 0).unwrap(), Some(peer1));
        assert_eq!(get_section_peer_suggestion(&conn, &hash, 1).unwrap(), Some(peer1));
        assert_eq!(get_section_peer_suggestion(&conn, &hash, 2).unwrap(), Some(peer2));
        assert_eq!(get_section_peer_suggestion(&conn, &hash, 3).unwrap(), Some(peer2));
        assert_eq!(get_section_peer_suggestion(&conn, &hash, 4).unwrap(), Some(peer3));
    }

    #[test]
    fn test_duplicate_recipient_ignored() {
        let conn = setup_db();
        let hash = [3u8; 32];
        let scope_id = [1u8; 32];
        let source_id = [4u8; 32];
        let peer1 = [5u8; 32];
        
        insert_blob(&conn, &hash, &scope_id, &source_id, "test.bin", 1024 * 1024, 3).unwrap();
        
        // Add same recipient twice - should not error (INSERT OR IGNORE)
        add_blob_recipient(&conn, &hash, &peer1).unwrap();
        add_blob_recipient(&conn, &hash, &peer1).unwrap();
        
        // Verify only one entry
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM blob_recipients WHERE hash = ?1",
            [hash.as_slice()],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }
}

