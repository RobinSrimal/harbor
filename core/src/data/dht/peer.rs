//! Peer data access layer
//!
//! Handles storage and retrieval of peer information including:
//! - EndpointID (32-byte public key)
//! - Latency measurements
//! - Last seen timestamps

use rusqlite::{Connection, OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum age (in seconds) for a peer to be retained before cleanup
/// 90 days = 3 months
pub const PEER_RETENTION_SECS: i64 = 90 * 24 * 60 * 60;

/// Peer information stored in the database
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// 32-byte EndpointID (ed25519 public key)
    pub endpoint_id: [u8; 32],
    /// Last measured latency in milliseconds
    pub last_latency_ms: Option<i64>,
    /// Timestamp of the last latency measurement
    pub latency_timestamp: Option<i64>,
    /// Timestamp when this peer was last seen
    pub last_seen: i64,
    /// Optional relay URL for this peer
    pub relay_url: Option<String>,
    /// Timestamp when the relay URL was last confirmed working
    pub relay_url_last_success: Option<i64>,
}

/// Get current Unix timestamp
///
/// Returns 0 if system clock is before Unix epoch (should never happen
/// on properly configured systems, but avoids panic).
pub fn current_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Insert or update a peer in the database
pub fn upsert_peer(conn: &Connection, peer: &PeerInfo) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO peers (endpoint_id, last_latency_ms, latency_timestamp, last_seen)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(endpoint_id) DO UPDATE SET
             last_latency_ms = COALESCE(?2, last_latency_ms),
             latency_timestamp = COALESCE(?3, latency_timestamp),
             last_seen = MAX(last_seen, excluded.last_seen)",
        params![
            peer.endpoint_id.as_slice(),
            peer.last_latency_ms,
            peer.latency_timestamp,
            peer.last_seen,
        ],
    )?;
    Ok(())
}

/// Get a peer by EndpointID
pub fn get_peer(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<Option<PeerInfo>> {
    conn.query_row(
        "SELECT endpoint_id, last_latency_ms, latency_timestamp, last_seen,
                relay_url, relay_url_last_success
         FROM peers WHERE endpoint_id = ?1",
        [endpoint_id.as_slice()],
        parse_peer_row,
    )
    .optional()
}

/// Parse a PeerInfo from a database row
fn parse_peer_row(row: &rusqlite::Row) -> rusqlite::Result<PeerInfo> {
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

    Ok(PeerInfo {
        endpoint_id,
        last_latency_ms: row.get(1)?,
        latency_timestamp: row.get(2)?,
        last_seen: row.get(3)?,
        relay_url: row.get(4)?,
        relay_url_last_success: row.get(5)?,
    })
}

/// Get a peer's relay URL and last success timestamp for connection routing
pub fn get_peer_relay_info(
    conn: &Connection,
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<Option<(String, Option<i64>)>> {
    conn.query_row(
        "SELECT relay_url, relay_url_last_success FROM peers WHERE endpoint_id = ?1",
        [endpoint_id.as_slice()],
        |row| {
            let relay_url: Option<String> = row.get(0)?;
            let last_success: Option<i64> = row.get(1)?;
            Ok(relay_url.map(|url| (url, last_success)))
        },
    )
    .optional()
    .map(|opt| opt.flatten())
}

/// Update a peer's relay URL and last success timestamp.
///
/// Only updates if the incoming timestamp is newer than what's stored:
/// - If only the timestamp is newer (same relay URL): updates just the timestamp.
/// - If the relay URL is also different: updates both.
/// - If the incoming timestamp is older or equal: no update.
///
/// Uses upsert to create the peer record if it doesn't exist yet.
pub fn update_peer_relay_url(
    conn: &Connection,
    endpoint_id: &[u8; 32],
    relay_url: &str,
    timestamp: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO peers (endpoint_id, relay_url, relay_url_last_success, last_seen)
         VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(endpoint_id) DO UPDATE SET
             relay_url = CASE
                 WHEN ?3 > COALESCE(relay_url_last_success, 0) THEN ?2
                 ELSE relay_url
             END,
             relay_url_last_success = CASE
                 WHEN ?3 > COALESCE(relay_url_last_success, 0) THEN ?3
                 ELSE relay_url_last_success
             END,
             last_seen = MAX(last_seen, ?3)",
        params![endpoint_id.as_slice(), relay_url, timestamp],
    )?;
    Ok(())
}

/// Store a relay URL for a peer without marking it as verified.
///
/// Only sets relay_url if the peer has no relay_url yet.
/// Does NOT set relay_url_last_success, so smart connect treats it as stale.
pub fn store_peer_relay_url_unverified(
    conn: &Connection,
    endpoint_id: &[u8; 32],
    relay_url: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE peers SET relay_url = ?2 WHERE endpoint_id = ?1 AND relay_url IS NULL",
        params![endpoint_id.as_slice(), relay_url],
    )?;
    Ok(())
}

/// Update peer's last seen timestamp
pub fn touch_peer(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let now = current_timestamp();
    let rows = conn.execute(
        "UPDATE peers SET last_seen = ?1 WHERE endpoint_id = ?2",
        params![now, endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Ensure a peer exists in the peers table (for FK constraints)
///
/// Uses INSERT OR IGNORE to create a minimal peer entry if it doesn't exist.
/// This is needed before inserting into tables with FK references to peers.
pub fn ensure_peer_exists(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<()> {
    let now = current_timestamp();
    conn.execute(
        "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
        params![endpoint_id.as_slice(), now],
    )?;
    Ok(())
}

/// Update peer's latency measurement
pub fn update_peer_latency(
    conn: &Connection,
    endpoint_id: &[u8; 32],
    latency_ms: i64,
) -> rusqlite::Result<bool> {
    let now = current_timestamp();
    let rows = conn.execute(
        "UPDATE peers SET last_latency_ms = ?1, latency_timestamp = ?2, last_seen = ?2 
         WHERE endpoint_id = ?3",
        params![latency_ms, now, endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Delete a peer by EndpointID
pub fn delete_peer(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM peers WHERE endpoint_id = ?1",
        [endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Cleanup peers not seen since `PEER_RETENTION_SECS` (3 months)
///
/// Removes stale peer records to prevent unbounded database growth.
/// Peers may reconnect later and will be re-added when seen.
pub fn cleanup_stale_peers(conn: &Connection) -> rusqlite::Result<usize> {
    let cutoff = current_timestamp() - PEER_RETENTION_SECS;
    let rows = conn.execute(
        "DELETE FROM peers
         WHERE last_seen < ?1
           AND id NOT IN (SELECT admin_peer_id FROM topics)
           AND id NOT IN (SELECT peer_id FROM topic_members)
           AND id NOT IN (SELECT peer_id FROM outgoing_recipients)
           AND id NOT IN (SELECT sender_peer_id FROM harbor_cache)
           AND id NOT IN (SELECT peer_id FROM harbor_recipients)
           AND id NOT IN (SELECT sender_peer_id FROM pending_decryption)
           AND id NOT IN (SELECT source_peer_id FROM blobs)
           AND id NOT IN (SELECT peer_id FROM blob_recipients)
           AND id NOT IN (SELECT received_from_peer_id FROM blob_sections WHERE received_from_peer_id IS NOT NULL)
           AND id NOT IN (SELECT peer_id FROM connection_list)
           AND id NOT IN (SELECT sender_peer_id FROM pending_topic_invites)
           AND id NOT IN (SELECT admin_peer_id FROM pending_topic_invites)
           AND id NOT IN (SELECT peer_id FROM harbor_nodes_cache)",
        [cutoff],
    )?;
    Ok(rows)
}

/// Get all peers not seen since the given timestamp
pub fn get_stale_peers(conn: &Connection, threshold: i64) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare("SELECT endpoint_id FROM peers WHERE last_seen < ?1")?;

    let peers = stmt
        .query_map([threshold], |row| {
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
            Ok(endpoint_id)
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(peers)
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

    fn test_endpoint_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_insert_and_get_peer() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(1);

        let peer = PeerInfo {
            endpoint_id,
            last_latency_ms: Some(50),
            latency_timestamp: Some(current_timestamp()),
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };

        upsert_peer(&conn, &peer).unwrap();

        let retrieved = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(retrieved.endpoint_id, endpoint_id);
        assert_eq!(retrieved.last_latency_ms, peer.last_latency_ms);
    }

    #[test]
    fn test_upsert_updates_existing() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(2);

        // Insert initial peer
        let peer1 = PeerInfo {
            endpoint_id,
            last_latency_ms: Some(50),
            latency_timestamp: Some(current_timestamp()),
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer1).unwrap();

        // Upsert without latency (should keep old value)
        let peer2 = PeerInfo {
            endpoint_id,
            last_latency_ms: None, // Should keep old value
            latency_timestamp: None,
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer2).unwrap();

        let retrieved = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        // Original latency should be preserved
        assert_eq!(retrieved.last_latency_ms, peer1.last_latency_ms);
    }

    #[test]
    fn test_touch_peer() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(6);

        let old_time = current_timestamp() - 1000;
        let peer = PeerInfo {
            endpoint_id,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: old_time,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer).unwrap();

        // Touch the peer
        touch_peer(&conn, &endpoint_id).unwrap();

        let retrieved = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        assert!(retrieved.last_seen > old_time);
    }

    #[test]
    fn test_delete_peer() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(8);

        let peer = PeerInfo {
            endpoint_id,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer).unwrap();

        assert!(get_peer(&conn, &endpoint_id).unwrap().is_some());

        delete_peer(&conn, &endpoint_id).unwrap();

        assert!(get_peer(&conn, &endpoint_id).unwrap().is_none());
    }

    #[test]
    fn test_get_stale_peers() {
        let conn = setup_db();
        let now = current_timestamp();

        // Fresh peer
        let fresh = PeerInfo {
            endpoint_id: test_endpoint_id(9),
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &fresh).unwrap();

        // Stale peer
        let stale = PeerInfo {
            endpoint_id: test_endpoint_id(10),
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now - 1000,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &stale).unwrap();

        let stale_peers = get_stale_peers(&conn, now - 500).unwrap();
        assert_eq!(stale_peers.len(), 1);
        assert_eq!(stale_peers[0], test_endpoint_id(10));
    }

    #[test]
    fn test_cleanup_stale_peers() {
        let conn = setup_db();
        let now = current_timestamp();

        // Recent peer (should be kept)
        let recent = PeerInfo {
            endpoint_id: test_endpoint_id(1),
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &recent).unwrap();

        // Stale peer (older than PEER_RETENTION_SECS, should be deleted)
        let stale = PeerInfo {
            endpoint_id: test_endpoint_id(2),
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now - PEER_RETENTION_SECS - 1000, // 3 months + buffer
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &stale).unwrap();

        // Verify both exist
        assert!(get_peer(&conn, &test_endpoint_id(1)).unwrap().is_some());
        assert!(get_peer(&conn, &test_endpoint_id(2)).unwrap().is_some());

        // Run cleanup
        let deleted = cleanup_stale_peers(&conn).unwrap();
        assert_eq!(deleted, 1);

        // Recent should remain, stale should be gone
        assert!(get_peer(&conn, &test_endpoint_id(1)).unwrap().is_some());
        assert!(get_peer(&conn, &test_endpoint_id(2)).unwrap().is_none());
    }

    #[test]
    fn test_upsert_peer_does_not_regress_last_seen() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(11);

        let newer = PeerInfo {
            endpoint_id,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: 2_000,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &newer).unwrap();

        let older = PeerInfo {
            endpoint_id,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: 1_000,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &older).unwrap();

        let stored = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(stored.last_seen, 2_000);
    }

    #[test]
    fn test_update_peer_relay_url_uses_newer_timestamp_only() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(12);
        ensure_peer_exists(&conn, &endpoint_id).unwrap();

        update_peer_relay_url(&conn, &endpoint_id, "https://relay.new/", 2_000).unwrap();
        let relay = get_peer_relay_info(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(relay.0, "https://relay.new/");
        assert_eq!(relay.1, Some(2_000));

        // Older timestamp should not replace relay metadata.
        update_peer_relay_url(&conn, &endpoint_id, "https://relay.old/", 1_500).unwrap();
        let relay = get_peer_relay_info(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(relay.0, "https://relay.new/");
        assert_eq!(relay.1, Some(2_000));
    }

    #[test]
    fn test_cleanup_stale_peers_skips_referenced_rows() {
        let conn = setup_db();
        crate::data::schema::create_topic_table(&conn).unwrap();

        let now = current_timestamp();
        let stale_seen = now - PEER_RETENTION_SECS - 1_000;

        let referenced_id = test_endpoint_id(13);
        let unreferenced_id = test_endpoint_id(14);

        upsert_peer(
            &conn,
            &PeerInfo {
                endpoint_id: referenced_id,
                last_latency_ms: None,
                latency_timestamp: None,
                last_seen: stale_seen,
                relay_url: None,
                relay_url_last_success: None,
            },
        )
        .unwrap();
        upsert_peer(
            &conn,
            &PeerInfo {
                endpoint_id: unreferenced_id,
                last_latency_ms: None,
                latency_timestamp: None,
                last_seen: stale_seen,
                relay_url: None,
                relay_url_last_success: None,
            },
        )
        .unwrap();

        // Keep referenced_id alive via topics.admin_peer_id FK.
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            params![
                [21u8; 32].as_slice(),
                [22u8; 32].as_slice(),
                referenced_id.as_slice(),
            ],
        )
        .unwrap();

        let deleted = cleanup_stale_peers(&conn).unwrap();
        assert_eq!(deleted, 1);

        assert!(get_peer(&conn, &referenced_id).unwrap().is_some());
        assert!(get_peer(&conn, &unreferenced_id).unwrap().is_none());
    }
}
