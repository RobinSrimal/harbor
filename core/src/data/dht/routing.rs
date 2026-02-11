//! DHT routing table data access layer
//!
//! Stores DHT routing information in the database as a backup/persistence layer.
//! The in-memory routing table is the primary source during operation.

use rusqlite::{Connection, params};

use crate::data::dht::peer::ensure_peer_exists;

/// DHT routing entry stored in database
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtEntry {
    /// 32-byte EndpointID
    pub endpoint_id: [u8; 32],
    /// Bucket index (0-255, where 0 is furthest, 255 is closest)
    pub bucket_index: u8,
    /// Timestamp when added to routing table
    pub added_at: i64,
}

/// Parse a DhtEntry from a database row
fn parse_dht_row(row: &rusqlite::Row) -> rusqlite::Result<DhtEntry> {
    let id_vec: Vec<u8> = row.get(0)?;

    if id_vec.len() != 32 {
        return Err(rusqlite::Error::InvalidColumnType(
            0,
            "endpoint_id".to_string(),
            rusqlite::types::Type::Blob,
        ));
    }

    let mut endpoint_id = [0u8; 32];
    endpoint_id.copy_from_slice(&id_vec);

    Ok(DhtEntry {
        endpoint_id,
        bucket_index: row.get::<_, i32>(1)? as u8,
        added_at: row.get(2)?,
    })
}

/// Insert a DHT routing entry
pub fn insert_dht_entry(conn: &Connection, entry: &DhtEntry) -> rusqlite::Result<()> {
    ensure_peer_exists(conn, &entry.endpoint_id)?;
    conn.execute(
        "INSERT OR REPLACE INTO dht_routing (peer_id, bucket_index, added_at)
         VALUES ((SELECT id FROM peers WHERE endpoint_id = ?1), ?2, ?3)",
        params![
            entry.endpoint_id.as_slice(),
            entry.bucket_index as i32,
            entry.added_at,
        ],
    )?;
    Ok(())
}

/// Remove a DHT routing entry
pub fn remove_dht_entry(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM dht_routing
         WHERE peer_id = (SELECT id FROM peers WHERE endpoint_id = ?1)",
        [endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Get all entries in a specific bucket
pub fn get_bucket_entries(conn: &Connection, bucket_index: u8) -> rusqlite::Result<Vec<DhtEntry>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id, r.bucket_index, r.added_at
         FROM dht_routing r
         JOIN peers p ON r.peer_id = p.id
         WHERE r.bucket_index = ?1
         ORDER BY r.added_at DESC",
    )?;

    let entries = stmt
        .query_map([bucket_index as i32], parse_dht_row)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries)
}

/// Get all DHT routing entries, grouped by bucket
pub fn get_all_entries(conn: &Connection) -> rusqlite::Result<Vec<DhtEntry>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id, r.bucket_index, r.added_at
         FROM dht_routing r
         JOIN peers p ON r.peer_id = p.id
         ORDER BY r.bucket_index, r.added_at DESC",
    )?;

    let entries = stmt
        .query_map([], parse_dht_row)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(entries)
}

/// Count entries in a bucket
pub fn count_bucket_entries(conn: &Connection, bucket_index: u8) -> rusqlite::Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dht_routing WHERE bucket_index = ?1",
        [bucket_index as i32],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Get total entry count across all buckets
pub fn count_all_entries(conn: &Connection) -> rusqlite::Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dht_routing",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Clear all DHT routing entries (useful for testing or reset)
pub fn clear_all_entries(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM dht_routing", [])?;
    Ok(())
}

/// Restore routing table from database entries
/// Returns entries grouped by bucket index (0-255)
pub fn load_routing_table(conn: &Connection) -> rusqlite::Result<Vec<Vec<[u8; 32]>>> {
    let mut buckets: Vec<Vec<[u8; 32]>> = vec![Vec::new(); 256];

    let entries = get_all_entries(conn)?;
    for entry in entries {
        // bucket_index is u8, so always 0-255 - direct indexing is safe
        buckets[entry.bucket_index as usize].push(entry.endpoint_id);
    }

    Ok(buckets)
}

/// Load relay URLs for routing table nodes from the peers table
///
/// Returns a list of (endpoint_id, relay_url) pairs for nodes that have
/// a relay URL stored in the peers table.
pub fn load_routing_table_relay_urls(conn: &Connection) -> rusqlite::Result<Vec<([u8; 32], String, Option<i64>)>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id, p.relay_url, p.relay_url_last_success
         FROM dht_routing r
         JOIN peers p ON r.peer_id = p.id
         WHERE p.relay_url IS NOT NULL",
    )?;

    let relay_urls = stmt
        .query_map([], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            let relay_url: String = row.get(1)?;
            let last_success: Option<i64> = row.get(2)?;
            if id_vec.len() != 32 {
                return Err(rusqlite::Error::InvalidColumnType(
                    0,
                    "endpoint_id".to_string(),
                    rusqlite::types::Type::Blob,
                ));
            }
            let mut endpoint_id = [0u8; 32];
            endpoint_id.copy_from_slice(&id_vec);
            Ok((endpoint_id, relay_url, last_success))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(relay_urls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_dht_table, create_peer_table};
    use crate::data::dht::peer::{current_timestamp, upsert_peer, PeerInfo};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_dht_table(&conn).unwrap();
        conn
    }

    fn create_test_peer(conn: &Connection, seed: u8) -> [u8; 32] {
        let endpoint_id = [seed; 32];
        let peer = PeerInfo {
            endpoint_id,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(conn, &peer).unwrap();
        endpoint_id
    }

    #[test]
    fn test_insert_and_get_entry() {
        let conn = setup_db();
        let endpoint_id = create_test_peer(&conn, 1);

        let entry = DhtEntry {
            endpoint_id,
            bucket_index: 42,
            added_at: current_timestamp(),
        };

        insert_dht_entry(&conn, &entry).unwrap();

        let entries = get_bucket_entries(&conn, 42).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].endpoint_id, endpoint_id);
        assert_eq!(entries[0].bucket_index, 42);
    }

    #[test]
    fn test_remove_entry() {
        let conn = setup_db();
        let endpoint_id = create_test_peer(&conn, 2);

        let entry = DhtEntry {
            endpoint_id,
            bucket_index: 10,
            added_at: current_timestamp(),
        };
        insert_dht_entry(&conn, &entry).unwrap();

        assert!(remove_dht_entry(&conn, &endpoint_id).unwrap());

        let entries = get_bucket_entries(&conn, 10).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_get_all_entries() {
        let conn = setup_db();

        // Add entries to multiple buckets
        for i in 0..5u8 {
            let endpoint_id = create_test_peer(&conn, i);
            let entry = DhtEntry {
                endpoint_id,
                bucket_index: i * 10,
                added_at: current_timestamp(),
            };
            insert_dht_entry(&conn, &entry).unwrap();
        }

        let entries = get_all_entries(&conn).unwrap();
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn test_count_bucket_entries() {
        let conn = setup_db();
        let bucket = 50u8;

        // Add 3 entries to the same bucket
        for i in 0..3u8 {
            let endpoint_id = create_test_peer(&conn, i);
            let entry = DhtEntry {
                endpoint_id,
                bucket_index: bucket,
                added_at: current_timestamp(),
            };
            insert_dht_entry(&conn, &entry).unwrap();
        }

        assert_eq!(count_bucket_entries(&conn, bucket).unwrap(), 3);
        assert_eq!(count_bucket_entries(&conn, 51).unwrap(), 0);
    }

    #[test]
    fn test_load_routing_table() {
        let conn = setup_db();

        // Add entries to buckets 0, 5, and 10
        for (i, bucket) in [0u8, 5, 10].iter().enumerate() {
            let endpoint_id = create_test_peer(&conn, i as u8);
            let entry = DhtEntry {
                endpoint_id,
                bucket_index: *bucket,
                added_at: current_timestamp(),
            };
            insert_dht_entry(&conn, &entry).unwrap();
        }

        let buckets = load_routing_table(&conn).unwrap();

        assert_eq!(buckets.len(), 256);
        assert_eq!(buckets[0].len(), 1);
        assert_eq!(buckets[5].len(), 1);
        assert_eq!(buckets[10].len(), 1);
        assert!(buckets[1].is_empty());
    }

    #[test]
    fn test_clear_all_entries() {
        let conn = setup_db();

        for i in 0..5u8 {
            let endpoint_id = create_test_peer(&conn, i);
            let entry = DhtEntry {
                endpoint_id,
                bucket_index: i,
                added_at: current_timestamp(),
            };
            insert_dht_entry(&conn, &entry).unwrap();
        }

        assert_eq!(count_all_entries(&conn).unwrap(), 5);

        clear_all_entries(&conn).unwrap();

        assert_eq!(count_all_entries(&conn).unwrap(), 0);
    }

    #[test]
    fn test_replace_on_insert() {
        let conn = setup_db();
        let endpoint_id = create_test_peer(&conn, 100);

        // Insert with bucket 10
        let entry1 = DhtEntry {
            endpoint_id,
            bucket_index: 10,
            added_at: 1000,
        };
        insert_dht_entry(&conn, &entry1).unwrap();

        // Insert same endpoint with bucket 20 (should replace)
        let entry2 = DhtEntry {
            endpoint_id,
            bucket_index: 20,
            added_at: 2000,
        };
        insert_dht_entry(&conn, &entry2).unwrap();

        // Should only have one entry total
        assert_eq!(count_all_entries(&conn).unwrap(), 1);

        // Should be in bucket 20
        let entries = get_bucket_entries(&conn, 20).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].endpoint_id, endpoint_id);

        // Bucket 10 should be empty
        let entries = get_bucket_entries(&conn, 10).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_load_routing_table_relay_urls() {
        let conn = setup_db();

        // Create peer with relay URL
        let endpoint_id = [1u8; 32];
        let peer = PeerInfo {
            endpoint_id,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: current_timestamp(),
            relay_url: Some("https://relay.example.com/".to_string()),
            relay_url_last_success: Some(current_timestamp()),
        };
        upsert_peer(&conn, &peer).unwrap();
        crate::data::update_peer_relay_url(&conn, &endpoint_id, "https://relay.example.com/", current_timestamp()).unwrap();

        let entry = DhtEntry {
            endpoint_id,
            bucket_index: 5,
            added_at: current_timestamp(),
        };
        insert_dht_entry(&conn, &entry).unwrap();

        // Create peer without relay URL
        let endpoint_id2 = create_test_peer(&conn, 2);
        let entry2 = DhtEntry {
            endpoint_id: endpoint_id2,
            bucket_index: 10,
            added_at: current_timestamp(),
        };
        insert_dht_entry(&conn, &entry2).unwrap();

        let relay_urls = load_routing_table_relay_urls(&conn).unwrap();
        assert_eq!(relay_urls.len(), 1);
        assert_eq!(relay_urls[0].0, endpoint_id);
        assert_eq!(relay_urls[0].1, "https://relay.example.com/");
        assert!(relay_urls[0].2.is_some(), "should have relay_url_last_success");
    }
}
