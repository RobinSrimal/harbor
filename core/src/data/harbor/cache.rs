//! Harbor Node data layer
//!
//! Manages cached packets for Harbor Nodes and tracks pulled packets for clients.
//!
//! # Tables
//!
//! - `harbor_cache`: Packets stored by this node as a Harbor Node
//! - `harbor_recipients`: Per-recipient delivery status for cached packets  
//! - `pulled_packets`: Packets we've pulled from Harbor Nodes

use rusqlite::{Connection, OptionalExtension, params};

use crate::data::dht::current_timestamp;

/// Maximum lifetime for cached packets (3 months in seconds)
pub const PACKET_LIFETIME_SECS: i64 = 90 * 24 * 60 * 60;

/// Packet type for verification mode
/// - Content (0): Full verification (MAC + signature)
/// - Join (1): MAC-only verification (sender key unknown to recipients)
/// - Leave (2): Full verification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Content = 0,
    Join = 1,
    Leave = 2,
}

impl PacketType {
    /// Check if this packet type requires signature verification
    pub fn requires_signature(&self) -> bool {
        match self {
            PacketType::Content => true,
            PacketType::Join => false,
            PacketType::Leave => true,
        }
    }
}

impl From<u8> for PacketType {
    fn from(value: u8) -> Self {
        match value {
            1 => PacketType::Join,
            2 => PacketType::Leave,
            _ => PacketType::Content,
        }
    }
}

impl From<PacketType> for u8 {
    fn from(value: PacketType) -> Self {
        value as u8
    }
}

/// A cached packet stored by a Harbor Node
#[derive(Debug, Clone)]
pub struct CachedPacket {
    /// Unique packet identifier
    pub packet_id: [u8; 32],
    /// HarborID this packet belongs to
    pub harbor_id: [u8; 32],
    /// Sender's EndpointID
    pub sender_id: [u8; 32],
    /// Serialized packet data
    pub packet_data: Vec<u8>,
    /// Type of packet (affects verification mode)
    pub packet_type: PacketType,
    /// Whether this was synced from another Harbor Node (vs original)
    pub synced: bool,
    /// When the packet was stored
    pub created_at: i64,
    /// When the packet expires
    pub expires_at: i64,
}

/// Parse a 32-byte ID from a database blob
fn parse_id(vec: &[u8], column_name: &str) -> rusqlite::Result<[u8; 32]> {
    if vec.len() != 32 {
        return Err(rusqlite::Error::InvalidColumnType(
            0,
            column_name.to_string(),
            rusqlite::types::Type::Blob,
        ));
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(vec);
    Ok(id)
}

/// Parse a CachedPacket from a database row
fn parse_cached_packet_row(row: &rusqlite::Row) -> rusqlite::Result<CachedPacket> {
    let packet_id_vec: Vec<u8> = row.get(0)?;
    let harbor_id_vec: Vec<u8> = row.get(1)?;
    let sender_id_vec: Vec<u8> = row.get(2)?;

    let packet_id = parse_id(&packet_id_vec, "packet_id")?;
    let harbor_id = parse_id(&harbor_id_vec, "harbor_id")?;
    let sender_id = parse_id(&sender_id_vec, "sender_id")?;

    let packet_type_u8: u8 = row.get(4)?;

    Ok(CachedPacket {
        packet_id,
        harbor_id,
        sender_id,
        packet_data: row.get(3)?,
        packet_type: PacketType::from(packet_type_u8),
        synced: row.get(5)?,
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
    })
}

/// Store a packet in the harbor cache
///
/// Called when:
/// - We receive a packet to store as a Harbor Node
/// - We sync a packet from another Harbor Node
///
/// This operation is atomic - either the packet and all recipients are stored, or none are.
pub fn cache_packet(
    conn: &mut Connection,
    packet_id: &[u8; 32],
    harbor_id: &[u8; 32],
    sender_id: &[u8; 32],
    packet_data: &[u8],
    packet_type: PacketType,
    recipients: &[[u8; 32]],
    synced: bool,
) -> rusqlite::Result<()> {
    cache_packet_with_ttl(conn, packet_id, harbor_id, sender_id, packet_data, packet_type, recipients, synced, None)
}

/// Store a packet in the harbor cache with an optional custom TTL
///
/// If `ttl_secs` is None, uses the default PACKET_LIFETIME_SECS (3 months).
pub fn cache_packet_with_ttl(
    conn: &mut Connection,
    packet_id: &[u8; 32],
    harbor_id: &[u8; 32],
    sender_id: &[u8; 32],
    packet_data: &[u8],
    packet_type: PacketType,
    recipients: &[[u8; 32]],
    synced: bool,
    ttl_secs: Option<i64>,
) -> rusqlite::Result<()> {
    let now = current_timestamp();
    let expires_at = now + ttl_secs.unwrap_or(PACKET_LIFETIME_SECS);

    let tx = conn.transaction()?;

    tx.execute(
        "INSERT OR REPLACE INTO harbor_cache 
         (packet_id, harbor_id, sender_id, packet_data, packet_type, synced, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            packet_id.as_slice(),
            harbor_id.as_slice(),
            sender_id.as_slice(),
            packet_data,
            u8::from(packet_type),
            synced,
            now,
            expires_at,
        ],
    )?;

    // Add recipients who haven't received the packet yet
    for recipient in recipients {
        tx.execute(
            "INSERT OR IGNORE INTO harbor_recipients (packet_id, recipient_id, delivered)
             VALUES (?1, ?2, 0)",
            params![packet_id.as_slice(), recipient.as_slice()],
        )?;
    }

    tx.commit()
}

/// Get a cached packet by ID
pub fn get_cached_packet(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<Option<CachedPacket>> {
    conn.query_row(
        "SELECT packet_id, harbor_id, sender_id, packet_data, packet_type, synced, created_at, expires_at
         FROM harbor_cache WHERE packet_id = ?1",
        [packet_id.as_slice()],
        |row| parse_cached_packet_row(row),
    )
    .optional()
}

/// Wildcard recipient constant - packets stored with this recipient
/// can be pulled by ANY member of the topic (used for catch-up mode)
pub const WILDCARD_RECIPIENT: [u8; 32] = [0xFF; 32];

/// Get all packets for a HarborID that a specific recipient hasn't received
///
/// Used when a client pulls missed packets.
/// Only returns packets created after `since_timestamp` (for new members).
/// Also returns packets stored with wildcard recipient (from senders in catch-up mode).
pub fn get_packets_for_recipient(
    conn: &Connection,
    harbor_id: &[u8; 32],
    recipient_id: &[u8; 32],
    since_timestamp: i64,
) -> rusqlite::Result<Vec<CachedPacket>> {
    // Query packets where recipient is either:
    // 1. The specific recipient_id, OR
    // 2. The wildcard recipient (catch-up mode packets available to all members)
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.packet_id, c.harbor_id, c.sender_id, c.packet_data, c.packet_type, c.synced, c.created_at, c.expires_at
         FROM harbor_cache c
         JOIN harbor_recipients r ON c.packet_id = r.packet_id
         WHERE c.harbor_id = ?1 
           AND (r.recipient_id = ?2 OR r.recipient_id = ?5)
           AND r.delivered = 0
           AND c.created_at >= ?3
           AND c.expires_at > ?4
         ORDER BY c.created_at ASC",
    )?;

    let now = current_timestamp();
    let packets = stmt
        .query_map(
            params![harbor_id.as_slice(), recipient_id.as_slice(), since_timestamp, now, WILDCARD_RECIPIENT.as_slice()],
            |row| parse_cached_packet_row(row),
        )?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(packets)
}

/// Mark a packet as delivered to a recipient
pub fn mark_delivered(
    conn: &Connection,
    packet_id: &[u8; 32],
    recipient_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "UPDATE harbor_recipients SET delivered = 1, delivered_at = ?1
         WHERE packet_id = ?2 AND recipient_id = ?3",
        params![current_timestamp(), packet_id.as_slice(), recipient_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Check if all recipients have received a packet
pub fn all_recipients_delivered(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let undelivered: i64 = conn.query_row(
        "SELECT COUNT(*) FROM harbor_recipients 
         WHERE packet_id = ?1 AND delivered = 0",
        [packet_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(undelivered == 0)
}

/// Get undelivered recipient IDs for a packet
pub fn get_undelivered_recipients(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT recipient_id FROM harbor_recipients 
         WHERE packet_id = ?1 AND delivered = 0",
    )?;

    let recipients = stmt
        .query_map([packet_id.as_slice()], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "recipient_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(recipients)
}

/// Get delivered recipient IDs for a packet
///
/// Used for sync protocol - share delivery status with other Harbor Nodes.
pub fn get_delivered_recipients(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT recipient_id FROM harbor_recipients 
         WHERE packet_id = ?1 AND delivered = 1",
    )?;

    let recipients = stmt
        .query_map([packet_id.as_slice()], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "recipient_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(recipients)
}

/// Get all packets we're storing as a Harbor Node for a specific HarborID
///
/// Used for syncing with other Harbor Nodes.
pub fn get_packets_for_sync(
    conn: &Connection,
    harbor_id: &[u8; 32],
) -> rusqlite::Result<Vec<CachedPacket>> {
    let now = current_timestamp();
    let mut stmt = conn.prepare(
        "SELECT packet_id, harbor_id, sender_id, packet_data, packet_type, synced, created_at, expires_at
         FROM harbor_cache 
         WHERE harbor_id = ?1 AND expires_at > ?2
         ORDER BY created_at ASC",
    )?;

    let packets = stmt
        .query_map(params![harbor_id.as_slice(), now], |row| parse_cached_packet_row(row))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(packets)
}

/// Get HarborIDs we're actively storing original (non-synced) packets for
///
/// These are the HarborIDs we consider ourselves a Harbor Node for.
pub fn get_active_harbor_ids(conn: &Connection) -> rusqlite::Result<Vec<[u8; 32]>> {
    let now = current_timestamp();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT harbor_id FROM harbor_cache 
         WHERE synced = 0 AND expires_at > ?1",
    )?;

    let ids = stmt
        .query_map([now], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "harbor_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ids)
}

/// Get harbor cache storage statistics
///
/// Returns (packet_count, total_bytes)
pub fn get_harbor_cache_stats(conn: &Connection) -> rusqlite::Result<(u32, u64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(packet_data)), 0) FROM harbor_cache",
        [],
        |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u64)),
    )
}

/// Delete expired packets
///
/// Recipients are automatically deleted via CASCADE foreign key.
pub fn cleanup_expired(conn: &Connection) -> rusqlite::Result<usize> {
    let now = current_timestamp();
    
    let rows = conn.execute(
        "DELETE FROM harbor_cache WHERE expires_at <= ?1",
        [now],
    )?;

    Ok(rows)
}

/// Delete a fully delivered packet
///
/// Recipients are automatically deleted via CASCADE foreign key.
pub fn delete_packet(conn: &Connection, packet_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM harbor_cache WHERE packet_id = ?1",
        [packet_id.as_slice()],
    )?;

    Ok(rows > 0)
}

// ============ Pulled Packets Tracking (Client Side) ============

/// Record that we pulled a packet from a Harbor Node
pub fn mark_pulled(
    conn: &Connection,
    topic_id: &[u8; 32],
    packet_id: &[u8; 32],
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO pulled_packets (topic_id, packet_id, pulled_at)
         VALUES (?1, ?2, ?3)",
        params![topic_id.as_slice(), packet_id.as_slice(), current_timestamp()],
    )?;
    Ok(())
}

/// Check if we've already pulled a packet
pub fn was_pulled(
    conn: &Connection,
    topic_id: &[u8; 32],
    packet_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pulled_packets 
         WHERE topic_id = ?1 AND packet_id = ?2",
        params![topic_id.as_slice(), packet_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get all pulled packet IDs for a topic
pub fn get_pulled_packet_ids(
    conn: &Connection,
    topic_id: &[u8; 32],
) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT packet_id FROM pulled_packets WHERE topic_id = ?1",
    )?;

    let ids = stmt
        .query_map([topic_id.as_slice()], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "packet_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ids)
}

/// Cleanup pulled packets for unsubscribed topics
pub fn cleanup_pulled_for_topic(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<usize> {
    let rows = conn.execute(
        "DELETE FROM pulled_packets WHERE topic_id = ?1",
        [topic_id.as_slice()],
    )?;
    Ok(rows)
}

/// Cleanup pulled packets older than `PACKET_LIFETIME_SECS`
///
/// Pulled packet IDs only need to be tracked as long as the corresponding
/// harbor packets could exist. After `PACKET_LIFETIME_SECS` (3 months),
/// the harbor packets are deleted, so we can clean up the tracking records.
pub fn cleanup_old_pulled_packets(conn: &Connection) -> rusqlite::Result<usize> {
    let cutoff = current_timestamp() - PACKET_LIFETIME_SECS;
    let rows = conn.execute(
        "DELETE FROM pulled_packets WHERE pulled_at < ?1",
        [cutoff],
    )?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_harbor_table;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_cache_packet() {
        let mut conn = setup_db();
        let packet_id = test_id(1);
        let harbor_id = test_id(2);
        let sender_id = test_id(3);
        let recipients = [test_id(10), test_id(11)];

        cache_packet(
            &mut conn,
            &packet_id,
            &harbor_id,
            &sender_id,
            b"packet data",
            PacketType::Content,
            &recipients,
            false,
        )
        .unwrap();

        let cached = get_cached_packet(&conn, &packet_id).unwrap().unwrap();
        assert_eq!(cached.packet_id, packet_id);
        assert_eq!(cached.harbor_id, harbor_id);
        assert_eq!(cached.sender_id, sender_id);
        assert_eq!(cached.packet_data, b"packet data");
        assert_eq!(cached.packet_type, PacketType::Content);
        assert!(!cached.synced);
    }

    #[test]
    fn test_get_packets_for_recipient() {
        let mut conn = setup_db();
        let harbor_id = test_id(5);
        let recipient = test_id(20);

        // Cache 2 packets
        cache_packet(
            &mut conn,
            &test_id(1),
            &harbor_id,
            &test_id(3),
            b"packet 1",
            PacketType::Content,
            &[recipient],
            false,
        )
        .unwrap();

        cache_packet(
            &mut conn,
            &test_id(2),
            &harbor_id,
            &test_id(4),
            b"packet 2",
            PacketType::Content,
            &[recipient],
            false,
        )
        .unwrap();

        let packets = get_packets_for_recipient(&conn, &harbor_id, &recipient, 0).unwrap();
        assert_eq!(packets.len(), 2);
    }

    #[test]
    fn test_mark_delivered() {
        let mut conn = setup_db();
        let packet_id = test_id(1);
        let recipient = test_id(10);

        cache_packet(
            &mut conn,
            &packet_id,
            &test_id(2),
            &test_id(3),
            b"data",
            PacketType::Content,
            &[recipient],
            false,
        )
        .unwrap();

        // Initially not delivered
        assert!(!all_recipients_delivered(&conn, &packet_id).unwrap());

        // Mark delivered
        mark_delivered(&conn, &packet_id, &recipient).unwrap();
        assert!(all_recipients_delivered(&conn, &packet_id).unwrap());
    }

    #[test]
    fn test_get_undelivered_recipients() {
        let mut conn = setup_db();
        let packet_id = test_id(1);
        let recipients = [test_id(10), test_id(11), test_id(12)];

        cache_packet(
            &mut conn,
            &packet_id,
            &test_id(2),
            &test_id(3),
            b"data",
            PacketType::Content,
            &recipients,
            false,
        )
        .unwrap();

        // Mark one as delivered
        mark_delivered(&conn, &packet_id, &recipients[0]).unwrap();

        let undelivered = get_undelivered_recipients(&conn, &packet_id).unwrap();
        assert_eq!(undelivered.len(), 2);
        assert!(!undelivered.contains(&recipients[0]));
    }

    #[test]
    fn test_get_delivered_recipients() {
        let mut conn = setup_db();
        let packet_id = test_id(1);
        let recipients = [test_id(10), test_id(11), test_id(12)];

        cache_packet(
            &mut conn,
            &packet_id,
            &test_id(2),
            &test_id(3),
            b"data",
            PacketType::Content,
            &recipients,
            false,
        )
        .unwrap();

        // Initially no delivered recipients
        let delivered = get_delivered_recipients(&conn, &packet_id).unwrap();
        assert!(delivered.is_empty());

        // Mark two as delivered
        mark_delivered(&conn, &packet_id, &recipients[0]).unwrap();
        mark_delivered(&conn, &packet_id, &recipients[1]).unwrap();

        let delivered = get_delivered_recipients(&conn, &packet_id).unwrap();
        assert_eq!(delivered.len(), 2);
        assert!(delivered.contains(&recipients[0]));
        assert!(delivered.contains(&recipients[1]));
        assert!(!delivered.contains(&recipients[2]));
    }

    #[test]
    fn test_get_active_harbor_ids() {
        let mut conn = setup_db();

        // Original packet
        cache_packet(
            &mut conn,
            &test_id(1),
            &test_id(100),
            &test_id(3),
            b"data",
            PacketType::Content,
            &[test_id(10)],
            false,
        )
        .unwrap();

        // Synced packet (different harbor_id)
        cache_packet(
            &mut conn,
            &test_id(2),
            &test_id(200),
            &test_id(4),
            b"data",
            PacketType::Content,
            &[test_id(11)],
            true,
        )
        .unwrap();

        let active = get_active_harbor_ids(&conn).unwrap();
        assert_eq!(active.len(), 1);
        assert!(active.contains(&test_id(100)));
    }

    #[test]
    fn test_delete_packet() {
        let mut conn = setup_db();
        let packet_id = test_id(1);

        cache_packet(
            &mut conn,
            &packet_id,
            &test_id(2),
            &test_id(3),
            b"data",
            PacketType::Content,
            &[test_id(10)],
            false,
        )
        .unwrap();

        assert!(get_cached_packet(&conn, &packet_id).unwrap().is_some());

        delete_packet(&conn, &packet_id).unwrap();
        assert!(get_cached_packet(&conn, &packet_id).unwrap().is_none());
    }

    #[test]
    fn test_mark_pulled() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let packet_id = test_id(2);

        assert!(!was_pulled(&conn, &topic_id, &packet_id).unwrap());

        mark_pulled(&conn, &topic_id, &packet_id).unwrap();
        assert!(was_pulled(&conn, &topic_id, &packet_id).unwrap());
    }

    #[test]
    fn test_get_pulled_packet_ids() {
        let conn = setup_db();
        let topic_id = test_id(1);

        mark_pulled(&conn, &topic_id, &test_id(10)).unwrap();
        mark_pulled(&conn, &topic_id, &test_id(11)).unwrap();
        mark_pulled(&conn, &topic_id, &test_id(12)).unwrap();

        let ids = get_pulled_packet_ids(&conn, &topic_id).unwrap();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn test_since_timestamp_filter() {
        let mut conn = setup_db();
        let harbor_id = test_id(5);
        let recipient = test_id(20);
        let now = current_timestamp();

        cache_packet(
            &mut conn,
            &test_id(1),
            &harbor_id,
            &test_id(3),
            b"old packet",
            PacketType::Content,
            &[recipient],
            false,
        )
        .unwrap();

        // Query with future timestamp - should get nothing
        let packets = get_packets_for_recipient(&conn, &harbor_id, &recipient, now + 1000).unwrap();
        assert_eq!(packets.len(), 0);

        // Query with past timestamp - should get packet
        let packets = get_packets_for_recipient(&conn, &harbor_id, &recipient, now - 1000).unwrap();
        assert_eq!(packets.len(), 1);
    }

    #[test]
    fn test_synced_vs_original() {
        let mut conn = setup_db();
        let harbor_id = test_id(5);

        // Store as original
        cache_packet(
            &mut conn,
            &test_id(1),
            &harbor_id,
            &test_id(3),
            b"original",
            PacketType::Content,
            &[],
            false,
        )
        .unwrap();

        // Store as synced
        cache_packet(
            &mut conn,
            &test_id(2),
            &harbor_id,
            &test_id(4),
            b"synced",
            PacketType::Content,
            &[],
            true,
        )
        .unwrap();

        let original = get_cached_packet(&conn, &test_id(1)).unwrap().unwrap();
        let synced = get_cached_packet(&conn, &test_id(2)).unwrap().unwrap();

        assert!(!original.synced);
        assert!(synced.synced);
    }

    #[test]
    fn test_packet_type_requires_signature() {
        assert!(PacketType::Content.requires_signature());
        assert!(!PacketType::Join.requires_signature());
        assert!(PacketType::Leave.requires_signature());
    }

    #[test]
    fn test_packet_type_conversions() {
        assert_eq!(PacketType::from(0), PacketType::Content);
        assert_eq!(PacketType::from(1), PacketType::Join);
        assert_eq!(PacketType::from(2), PacketType::Leave);
        // Unknown values default to Content
        assert_eq!(PacketType::from(255), PacketType::Content);
        
        assert_eq!(u8::from(PacketType::Content), 0);
        assert_eq!(u8::from(PacketType::Join), 1);
        assert_eq!(u8::from(PacketType::Leave), 2);
    }

    #[test]
    fn test_cascade_delete_recipients() {
        let mut conn = setup_db();
        let packet_id = test_id(1);
        let recipients = [test_id(10), test_id(11)];

        cache_packet(
            &mut conn,
            &packet_id,
            &test_id(2),
            &test_id(3),
            b"data",
            PacketType::Content,
            &recipients,
            false,
        )
        .unwrap();

        // Verify recipients exist
        let undelivered = get_undelivered_recipients(&conn, &packet_id).unwrap();
        assert_eq!(undelivered.len(), 2);

        // Delete packet (should cascade to recipients)
        delete_packet(&conn, &packet_id).unwrap();

        // Verify recipients are gone
        let undelivered = get_undelivered_recipients(&conn, &packet_id).unwrap();
        assert!(undelivered.is_empty());
    }

    #[test]
    fn test_cleanup_pulled_for_topic() {
        let conn = setup_db();
        let topic1 = test_id(1);
        let topic2 = test_id(2);
        let packet1 = test_id(10);
        let packet2 = test_id(11);
        let packet3 = test_id(12);

        // Mark some packets as pulled for two topics
        mark_pulled(&conn, &topic1, &packet1).unwrap();
        mark_pulled(&conn, &topic1, &packet2).unwrap();
        mark_pulled(&conn, &topic2, &packet3).unwrap();

        // Verify they exist
        assert!(was_pulled(&conn, &topic1, &packet1).unwrap());
        assert!(was_pulled(&conn, &topic1, &packet2).unwrap());
        assert!(was_pulled(&conn, &topic2, &packet3).unwrap());

        // Cleanup topic1's pulled packets
        let deleted = cleanup_pulled_for_topic(&conn, &topic1).unwrap();
        assert_eq!(deleted, 2);

        // Verify topic1's are gone, topic2's remain
        assert!(!was_pulled(&conn, &topic1, &packet1).unwrap());
        assert!(!was_pulled(&conn, &topic1, &packet2).unwrap());
        assert!(was_pulled(&conn, &topic2, &packet3).unwrap());
    }

    #[test]
    fn test_cleanup_old_pulled_packets() {
        let conn = setup_db();
        let topic = test_id(1);
        let packet = test_id(10);

        // Mark a packet as pulled
        mark_pulled(&conn, &topic, &packet).unwrap();
        assert!(was_pulled(&conn, &topic, &packet).unwrap());

        // Run cleanup - should not delete recent packets
        let deleted = cleanup_old_pulled_packets(&conn).unwrap();
        assert_eq!(deleted, 0);
        assert!(was_pulled(&conn, &topic, &packet).unwrap());

        // Manually insert an old pulled packet (older than PACKET_LIFETIME_SECS)
        let old_packet = test_id(11);
        let old_timestamp = current_timestamp() - PACKET_LIFETIME_SECS - 1000;
        conn.execute(
            "INSERT INTO pulled_packets (topic_id, packet_id, pulled_at) VALUES (?1, ?2, ?3)",
            params![topic.as_slice(), old_packet.as_slice(), old_timestamp],
        ).unwrap();

        // Verify it was inserted
        assert!(was_pulled(&conn, &topic, &old_packet).unwrap());

        // Run cleanup - should delete the old one
        let deleted = cleanup_old_pulled_packets(&conn).unwrap();
        assert_eq!(deleted, 1);

        // Recent packet should remain, old one gone
        assert!(was_pulled(&conn, &topic, &packet).unwrap());
        assert!(!was_pulled(&conn, &topic, &old_packet).unwrap());
    }
}

