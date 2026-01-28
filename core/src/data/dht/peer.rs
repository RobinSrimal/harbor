//! Peer data access layer
//!
//! Handles storage and retrieval of peer information including:
//! - EndpointID (32-byte public key)
//! - EndpointAddress (IP:port for direct dialing)
//! - Latency measurements
//! - Last seen timestamps

use rusqlite::{Connection, OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum age (in seconds) for an address to be considered fresh for direct dialing
/// 24 hours = 86400 seconds
pub const ADDRESS_FRESHNESS_THRESHOLD_SECS: i64 = 86400;

/// Maximum age (in seconds) for a peer to be retained before cleanup
/// 90 days = 3 months
pub const PEER_RETENTION_SECS: i64 = 90 * 24 * 60 * 60;

/// Peer information stored in the database
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// 32-byte EndpointID (ed25519 public key)
    pub endpoint_id: [u8; 32],
    /// Optional endpoint address for direct dialing
    pub endpoint_address: Option<String>,
    /// Timestamp when the address was last updated
    pub address_timestamp: Option<i64>,
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

impl PeerInfo {
    /// Check if the endpoint address is fresh enough for direct dialing
    /// (less than 24 hours old)
    pub fn has_fresh_address(&self) -> bool {
        if let (Some(addr), Some(timestamp)) = (&self.endpoint_address, self.address_timestamp) {
            if addr.is_empty() {
                return false;
            }
            let now = current_timestamp();
            now - timestamp < ADDRESS_FRESHNESS_THRESHOLD_SECS
        } else {
            false
        }
    }
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
        "INSERT INTO peers (endpoint_id, endpoint_address, address_timestamp, 
                           last_latency_ms, latency_timestamp, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(endpoint_id) DO UPDATE SET
             endpoint_address = COALESCE(?2, endpoint_address),
             address_timestamp = COALESCE(?3, address_timestamp),
             last_latency_ms = COALESCE(?4, last_latency_ms),
             latency_timestamp = COALESCE(?5, latency_timestamp),
             last_seen = ?6",
        params![
            peer.endpoint_id.as_slice(),
            peer.endpoint_address,
            peer.address_timestamp,
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
        "SELECT endpoint_id, endpoint_address, address_timestamp,
                last_latency_ms, latency_timestamp, last_seen,
                relay_url, relay_url_last_success
         FROM peers WHERE endpoint_id = ?1",
        [endpoint_id.as_slice()],
        |row| parse_peer_row(row),
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
        endpoint_address: row.get(1)?,
        address_timestamp: row.get(2)?,
        last_latency_ms: row.get(3)?,
        latency_timestamp: row.get(4)?,
        last_seen: row.get(5)?,
        relay_url: row.get(6)?,
        relay_url_last_success: row.get(7)?,
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

/// Update peer's endpoint address
pub fn update_peer_address(
    conn: &Connection,
    endpoint_id: &[u8; 32],
    address: &str,
) -> rusqlite::Result<bool> {
    let now = current_timestamp();
    let rows = conn.execute(
        "UPDATE peers SET endpoint_address = ?1, address_timestamp = ?2, last_seen = ?2 
         WHERE endpoint_id = ?3",
        params![address, now, endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
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
        "DELETE FROM peers WHERE last_seen < ?1",
        [cutoff],
    )?;
    Ok(rows)
}

/// Get all peers not seen since the given timestamp
pub fn get_stale_peers(conn: &Connection, threshold: i64) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT endpoint_id FROM peers WHERE last_seen < ?1",
    )?;
    
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

/// Get peers with fresh addresses for direct dialing
pub fn get_peers_with_fresh_addresses(conn: &Connection) -> rusqlite::Result<Vec<PeerInfo>> {
    let threshold = current_timestamp() - ADDRESS_FRESHNESS_THRESHOLD_SECS;

    let mut stmt = conn.prepare(
        "SELECT endpoint_id, endpoint_address, address_timestamp,
                last_latency_ms, latency_timestamp, last_seen,
                relay_url, relay_url_last_success
         FROM peers
         WHERE endpoint_address IS NOT NULL
           AND address_timestamp IS NOT NULL
           AND address_timestamp > ?1",
    )?;
    
    let peers = stmt
        .query_map([threshold], |row| parse_peer_row(row))?
        .collect::<Result<Vec<_>, _>>()?;
    
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_peer_table;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_peer_table(&conn).unwrap();
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
            endpoint_address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(current_timestamp()),
            last_latency_ms: Some(50),
            latency_timestamp: Some(current_timestamp()),
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };

        upsert_peer(&conn, &peer).unwrap();

        let retrieved = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(retrieved.endpoint_id, endpoint_id);
        assert_eq!(retrieved.endpoint_address, peer.endpoint_address);
        assert_eq!(retrieved.last_latency_ms, peer.last_latency_ms);
    }

    #[test]
    fn test_upsert_updates_existing() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(2);

        // Insert initial peer
        let peer1 = PeerInfo {
            endpoint_id,
            endpoint_address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(current_timestamp()),
            last_latency_ms: Some(50),
            latency_timestamp: Some(current_timestamp()),
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer1).unwrap();

        // Upsert with new address
        let peer2 = PeerInfo {
            endpoint_id,
            endpoint_address: Some("10.0.0.1:4433".to_string()),
            address_timestamp: Some(current_timestamp()),
            last_latency_ms: None, // Should keep old value
            latency_timestamp: None,
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer2).unwrap();
        
        let retrieved = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(retrieved.endpoint_address, peer2.endpoint_address);
        // Original latency should be preserved
        assert_eq!(retrieved.last_latency_ms, peer1.last_latency_ms);
    }

    #[test]
    fn test_has_fresh_address() {
        let now = current_timestamp();
        
        // Fresh address (1 hour old)
        let fresh_peer = PeerInfo {
            endpoint_id: test_endpoint_id(3),
            endpoint_address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now - 3600), // 1 hour ago
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        assert!(fresh_peer.has_fresh_address());

        // Stale address (25 hours old)
        let stale_peer = PeerInfo {
            endpoint_id: test_endpoint_id(4),
            endpoint_address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now - 90000), // 25 hours ago
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        assert!(!stale_peer.has_fresh_address());

        // No address
        let no_addr_peer = PeerInfo {
            endpoint_id: test_endpoint_id(5),
            endpoint_address: None,
            address_timestamp: None,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        assert!(!no_addr_peer.has_fresh_address());
    }

    #[test]
    fn test_touch_peer() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(6);
        
        let old_time = current_timestamp() - 1000;
        let peer = PeerInfo {
            endpoint_id,
            endpoint_address: None,
            address_timestamp: None,
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
    fn test_update_peer_address() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(7);
        
        let peer = PeerInfo {
            endpoint_id,
            endpoint_address: None,
            address_timestamp: None,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: current_timestamp(),
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &peer).unwrap();

        update_peer_address(&conn, &endpoint_id, "10.0.0.1:4433").unwrap();
        
        let retrieved = get_peer(&conn, &endpoint_id).unwrap().unwrap();
        assert_eq!(retrieved.endpoint_address, Some("10.0.0.1:4433".to_string()));
        assert!(retrieved.address_timestamp.is_some());
    }

    #[test]
    fn test_delete_peer() {
        let conn = setup_db();
        let endpoint_id = test_endpoint_id(8);
        
        let peer = PeerInfo {
            endpoint_id,
            endpoint_address: None,
            address_timestamp: None,
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
            endpoint_address: None,
            address_timestamp: None,
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
            endpoint_address: None,
            address_timestamp: None,
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
    fn test_get_peers_with_fresh_addresses() {
        let conn = setup_db();
        let now = current_timestamp();
        
        // Peer with fresh address
        let fresh = PeerInfo {
            endpoint_id: test_endpoint_id(11),
            endpoint_address: Some("192.168.1.1:4433".to_string()),
            address_timestamp: Some(now - 3600), // 1 hour ago
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &fresh).unwrap();

        // Peer with stale address
        let stale = PeerInfo {
            endpoint_id: test_endpoint_id(12),
            endpoint_address: Some("192.168.1.2:4433".to_string()),
            address_timestamp: Some(now - 100000), // > 24 hours ago
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &stale).unwrap();

        // Peer with no address
        let no_addr = PeerInfo {
            endpoint_id: test_endpoint_id(13),
            endpoint_address: None,
            address_timestamp: None,
            last_latency_ms: None,
            latency_timestamp: None,
            last_seen: now,
            relay_url: None,
            relay_url_last_success: None,
        };
        upsert_peer(&conn, &no_addr).unwrap();
        
        let fresh_peers = get_peers_with_fresh_addresses(&conn).unwrap();
        assert_eq!(fresh_peers.len(), 1);
        assert_eq!(fresh_peers[0].endpoint_id, test_endpoint_id(11));
    }

    #[test]
    fn test_cleanup_stale_peers() {
        let conn = setup_db();
        let now = current_timestamp();

        // Recent peer (should be kept)
        let recent = PeerInfo {
            endpoint_id: test_endpoint_id(1),
            endpoint_address: None,
            address_timestamp: None,
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
            endpoint_address: None,
            address_timestamp: None,
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
}

