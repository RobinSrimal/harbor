//! Outgoing packet tracking
//!
//! Tracks sent packets and their delivery status:
//! - Which recipients have acknowledged (read receipts)
//! - Whether packet has been replicated to Harbor Nodes

use rusqlite::{Connection, OptionalExtension, params};

use crate::data::dht::{current_timestamp, ensure_peer_exists};

/// Outgoing packet record
#[derive(Debug, Clone)]
pub struct OutgoingPacket {
    /// Unique packet identifier (32 bytes)
    pub packet_id: [u8; 32],
    /// Topic this packet belongs to
    pub topic_id: [u8; 32],
    /// Harbor ID (hash of topic)
    pub harbor_id: [u8; 32],
    /// Serialized packet data
    pub packet_data: Vec<u8>,
    /// When the packet was created
    pub created_at: i64,
    /// Whether replicated to Harbor Nodes
    pub replicated_to_harbor: bool,
    /// Packet type: 0=Content, 1=Join, 2=Leave
    pub packet_type: u8,
}

/// Recipient delivery status
#[derive(Debug, Clone)]
pub struct RecipientStatus {
    /// Packet ID
    pub packet_id: [u8; 32],
    /// Recipient's EndpointID
    pub endpoint_id: [u8; 32],
    /// Whether recipient has acknowledged
    pub acknowledged: bool,
    /// When acknowledged (if applicable)
    pub acknowledged_at: Option<i64>,
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

/// Parse an OutgoingPacket from a database row
fn parse_outgoing_packet_row(row: &rusqlite::Row) -> rusqlite::Result<OutgoingPacket> {
    let packet_id_vec: Vec<u8> = row.get(0)?;
    let topic_id_vec: Vec<u8> = row.get(1)?;
    let harbor_id_vec: Vec<u8> = row.get(2)?;

    let packet_id = parse_id(&packet_id_vec, "packet_id")?;
    let topic_id = parse_id(&topic_id_vec, "topic_id")?;
    let harbor_id = parse_id(&harbor_id_vec, "harbor_id")?;

    Ok(OutgoingPacket {
        packet_id,
        topic_id,
        harbor_id,
        packet_data: row.get(3)?,
        created_at: row.get(4)?,
        replicated_to_harbor: row.get::<_, i32>(5)? != 0,
        packet_type: row.get::<_, i32>(6)? as u8,
    })
}

/// Store a new outgoing packet with its recipients
///
/// This operation is atomic - either the packet and all recipients are stored, or none are.
/// packet_type: 0=Content, 1=Join, 2=Leave (matches HarborPacketType)
pub fn store_outgoing_packet(
    conn: &mut Connection,
    packet_id: &[u8; 32],
    topic_id: &[u8; 32],
    harbor_id: &[u8; 32],
    packet_data: &[u8],
    recipients: &[[u8; 32]],
    packet_type: u8,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;

    // Insert packet
    tx.execute(
        "INSERT INTO outgoing_packets (packet_id, topic_id, harbor_id, packet_data, packet_type)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            packet_id.as_slice(),
            topic_id.as_slice(),
            harbor_id.as_slice(),
            packet_data,
            packet_type as i32,
        ],
    )?;

    // Insert recipients
    for recipient in recipients {
        ensure_peer_exists(&tx, recipient)?;
        tx.execute(
            "INSERT INTO outgoing_recipients (packet_id, peer_id)
             VALUES (?1, (SELECT id FROM peers WHERE endpoint_id = ?2))",
            params![packet_id.as_slice(), recipient.as_slice()],
        )?;
    }

    tx.commit()
}

/// Get an outgoing packet by ID
pub fn get_outgoing_packet(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<Option<OutgoingPacket>> {
    conn.query_row(
        "SELECT packet_id, topic_id, harbor_id, packet_data, created_at, replicated_to_harbor, packet_type
         FROM outgoing_packets WHERE packet_id = ?1",
        [packet_id.as_slice()],
        parse_outgoing_packet_row,
    )
    .optional()
}

/// Record a read receipt from a recipient
pub fn acknowledge_receipt(
    conn: &Connection,
    packet_id: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let now = current_timestamp();
    let rows = conn.execute(
        "UPDATE outgoing_recipients 
         SET acknowledged = 1, acknowledged_at = ?1
         WHERE packet_id = ?2
           AND peer_id = (SELECT id FROM peers WHERE endpoint_id = ?3)
           AND acknowledged = 0",
        params![now, packet_id.as_slice(), endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Check if all recipients have acknowledged a packet
pub fn all_recipients_acknowledged(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let unacked_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM outgoing_recipients 
         WHERE packet_id = ?1 AND acknowledged = 0",
        [packet_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(unacked_count == 0)
}

/// Record a read receipt and delete packet if all recipients have acknowledged
/// Returns: (was_new_ack, was_deleted)
pub fn acknowledge_and_cleanup_if_complete(
    conn: &Connection,
    packet_id: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<(bool, bool)> {
    // Record the ack
    let was_new_ack = acknowledge_receipt(conn, packet_id, endpoint_id)?;
    
    // Check if all recipients have now acked
    if all_recipients_acknowledged(conn, packet_id)? {
        // All done - delete the packet
        delete_outgoing_packet(conn, packet_id)?;
        Ok((was_new_ack, true))
    } else {
        Ok((was_new_ack, false))
    }
}

/// Get packets that need harbor replication (not all recipients acknowledged)
pub fn get_packets_needing_replication(
    conn: &Connection,
) -> rusqlite::Result<Vec<OutgoingPacket>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT p.packet_id, p.topic_id, p.harbor_id, p.packet_data, p.created_at, p.replicated_to_harbor, p.packet_type
         FROM outgoing_packets p
         JOIN outgoing_recipients r ON p.packet_id = r.packet_id
         WHERE p.replicated_to_harbor = 0 AND r.acknowledged = 0",
    )?;

    let packets = stmt
        .query_map([], parse_outgoing_packet_row)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(packets)
}

/// Mark a packet as replicated to Harbor Nodes
pub fn mark_replicated_to_harbor(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "UPDATE outgoing_packets SET replicated_to_harbor = 1 WHERE packet_id = ?1",
        [packet_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Get unacknowledged recipients for a packet
pub fn get_unacknowledged_recipients(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id
         FROM outgoing_recipients r
         JOIN peers p ON r.peer_id = p.id
         WHERE r.packet_id = ?1 AND r.acknowledged = 0",
    )?;

    let recipients = stmt
        .query_map([packet_id.as_slice()], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "endpoint_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(recipients)
}

/// Delete a packet and its recipients (cascade)
pub fn delete_outgoing_packet(
    conn: &Connection,
    packet_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM outgoing_packets WHERE packet_id = ?1",
        [packet_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Clean up old packets that are fully acknowledged
pub fn cleanup_acknowledged_packets(
    conn: &Connection,
    older_than: i64,
) -> rusqlite::Result<usize> {
    // Find packets where all recipients have acknowledged and are old enough
    let deleted = conn.execute(
        "DELETE FROM outgoing_packets 
         WHERE created_at < ?1
         AND packet_id NOT IN (
             SELECT packet_id FROM outgoing_recipients WHERE acknowledged = 0
         )",
        [older_than],
    )?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_outgoing_table, create_peer_table};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_outgoing_table(&conn).unwrap();
        conn
    }

    fn ensure_test_peer(conn: &Connection, endpoint_id: &[u8; 32]) {
        conn.execute(
            "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![endpoint_id.as_slice(), 1704067200i64],
        )
        .unwrap();
    }

    fn test_packet_id() -> [u8; 32] {
        [1u8; 32]
    }

    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }

    fn test_harbor_id() -> [u8; 32] {
        [99u8; 32]
    }

    fn test_recipients() -> Vec<[u8; 32]> {
        vec![[10u8; 32], [20u8; 32], [30u8; 32]]
    }

    #[test]
    fn test_store_and_get_packet() {
        let mut conn = setup_db();
        let packet_id = test_packet_id();
        let topic_id = test_topic_id();
        let harbor_id = test_harbor_id();
        let packet_data = b"encrypted packet data";
        let recipients = test_recipients();

        for r in &recipients {
            ensure_test_peer(&conn, r);
        }

        store_outgoing_packet(
            &mut conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            packet_data,
            &recipients,
            0, // Content
        )
        .unwrap();

        let packet = get_outgoing_packet(&conn, &packet_id).unwrap().unwrap();
        
        assert_eq!(packet.packet_id, packet_id);
        assert_eq!(packet.topic_id, topic_id);
        assert_eq!(packet.harbor_id, harbor_id);
        assert_eq!(packet.packet_data, packet_data);
        assert!(!packet.replicated_to_harbor);
        assert_eq!(packet.packet_type, 0);
    }

    #[test]
    fn test_acknowledge_receipt() {
        let mut conn = setup_db();
        let packet_id = test_packet_id();
        let recipients = test_recipients();

        for r in &recipients {
            ensure_test_peer(&conn, r);
        }

        store_outgoing_packet(
            &mut conn,
            &packet_id,
            &test_topic_id(),
            &test_harbor_id(),
            b"data",
            &recipients,
            0, // Content
        )
        .unwrap();

        // Initially not all acknowledged
        assert!(!all_recipients_acknowledged(&conn, &packet_id).unwrap());

        // Acknowledge one
        assert!(acknowledge_receipt(&conn, &packet_id, &recipients[0]).unwrap());
        assert!(!all_recipients_acknowledged(&conn, &packet_id).unwrap());

        // Acknowledge rest
        acknowledge_receipt(&conn, &packet_id, &recipients[1]).unwrap();
        acknowledge_receipt(&conn, &packet_id, &recipients[2]).unwrap();

        // Now all acknowledged
        assert!(all_recipients_acknowledged(&conn, &packet_id).unwrap());
    }

    #[test]
    fn test_get_unacknowledged_recipients() {
        let mut conn = setup_db();
        let packet_id = test_packet_id();
        let recipients = test_recipients();

        store_outgoing_packet(
            &mut conn,
            &packet_id,
            &test_topic_id(),
            &test_harbor_id(),
            b"data",
            &recipients,
            0, // Content
        )
        .unwrap();

        // Initially all unacknowledged
        let unacked = get_unacknowledged_recipients(&conn, &packet_id).unwrap();
        assert_eq!(unacked.len(), 3);

        // Acknowledge one
        acknowledge_receipt(&conn, &packet_id, &recipients[0]).unwrap();

        let unacked = get_unacknowledged_recipients(&conn, &packet_id).unwrap();
        assert_eq!(unacked.len(), 2);
        assert!(!unacked.contains(&recipients[0]));
    }

    #[test]
    fn test_packets_needing_replication() {
        let mut conn = setup_db();
        
        // Packet with unacked recipients
        let packet1 = [1u8; 32];
        ensure_test_peer(&conn, &[10u8; 32]);
        store_outgoing_packet(
            &mut conn,
            &packet1,
            &test_topic_id(),
            &test_harbor_id(),
            b"data1",
            &[[10u8; 32]],
            0, // Content
        )
        .unwrap();

        // Packet with all acked
        let packet2 = [2u8; 32];
        ensure_test_peer(&conn, &[20u8; 32]);
        store_outgoing_packet(
            &mut conn,
            &packet2,
            &test_topic_id(),
            &test_harbor_id(),
            b"data2",
            &[[20u8; 32]],
            0, // Content
        )
        .unwrap();
        acknowledge_receipt(&conn, &packet2, &[20u8; 32]).unwrap();

        let needing_replication = get_packets_needing_replication(&conn).unwrap();
        assert_eq!(needing_replication.len(), 1);
        assert_eq!(needing_replication[0].packet_id, packet1);
    }

    #[test]
    fn test_mark_replicated() {
        let mut conn = setup_db();
        let packet_id = test_packet_id();
        ensure_test_peer(&conn, &[10u8; 32]);

        store_outgoing_packet(
            &mut conn,
            &packet_id,
            &test_topic_id(),
            &test_harbor_id(),
            b"data",
            &[[10u8; 32]],
            0, // Content
        )
        .unwrap();

        let packet = get_outgoing_packet(&conn, &packet_id).unwrap().unwrap();
        assert!(!packet.replicated_to_harbor);

        mark_replicated_to_harbor(&conn, &packet_id).unwrap();

        let packet = get_outgoing_packet(&conn, &packet_id).unwrap().unwrap();
        assert!(packet.replicated_to_harbor);

        // Should no longer appear in needing replication
        let needing = get_packets_needing_replication(&conn).unwrap();
        assert!(needing.is_empty());
    }

    #[test]
    fn test_delete_packet_cascades() {
        let mut conn = setup_db();
        let packet_id = test_packet_id();
        let recipients = test_recipients();

        for r in &recipients {
            ensure_test_peer(&conn, r);
        }

        store_outgoing_packet(
            &mut conn,
            &packet_id,
            &test_topic_id(),
            &test_harbor_id(),
            b"data",
            &recipients,
            0, // Content
        )
        .unwrap();

        // Recipients exist
        let unacked = get_unacknowledged_recipients(&conn, &packet_id).unwrap();
        assert_eq!(unacked.len(), 3);

        // Delete packet
        delete_outgoing_packet(&conn, &packet_id).unwrap();

        // Packet gone
        assert!(get_outgoing_packet(&conn, &packet_id).unwrap().is_none());

        // Recipients should be cascade deleted
        let unacked = get_unacknowledged_recipients(&conn, &packet_id).unwrap();
        assert!(unacked.is_empty());
    }

    #[test]
    fn test_cleanup_acknowledged() {
        let conn = setup_db();
        let now = current_timestamp();
        let old_time = now - 1000;

        // Old packet, all acknowledged
        let old_acked = [1u8; 32];
        ensure_test_peer(&conn, &[10u8; 32]);
        conn.execute(
            "INSERT INTO outgoing_packets (packet_id, topic_id, harbor_id, packet_data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                old_acked.as_slice(),
                test_topic_id().as_slice(),
                test_harbor_id().as_slice(),
                b"data".as_slice(),
                old_time,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outgoing_recipients (packet_id, peer_id, acknowledged)
             VALUES (?1, (SELECT id FROM peers WHERE endpoint_id = ?2), 1)",
            params![old_acked.as_slice(), [10u8; 32].as_slice()],
        )
        .unwrap();

        // Old packet, not acknowledged - should NOT be cleaned
        let old_unacked = [2u8; 32];
        ensure_test_peer(&conn, &[20u8; 32]);
        conn.execute(
            "INSERT INTO outgoing_packets (packet_id, topic_id, harbor_id, packet_data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                old_unacked.as_slice(),
                test_topic_id().as_slice(),
                test_harbor_id().as_slice(),
                b"data".as_slice(),
                old_time,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outgoing_recipients (packet_id, peer_id, acknowledged)
             VALUES (?1, (SELECT id FROM peers WHERE endpoint_id = ?2), 0)",
            params![old_unacked.as_slice(), [20u8; 32].as_slice()],
        )
        .unwrap();

        // Cleanup
        let deleted = cleanup_acknowledged_packets(&conn, now - 500).unwrap();
        assert_eq!(deleted, 1);

        // Old acked should be gone
        assert!(get_outgoing_packet(&conn, &old_acked).unwrap().is_none());

        // Old unacked should remain
        assert!(get_outgoing_packet(&conn, &old_unacked).unwrap().is_some());
    }
}
