//! Packet Deduplication
//!
//! Ensures packets aren't processed twice, which is important because:
//! - Direct delivery and Harbor pull can both deliver the same packet
//! - Multiple Harbor Nodes may store the same packet (redundancy)
//! - Network retries may cause duplicate deliveries
//!
//! # Deduplication Flow
//!
//! 1. Receive packet (direct or via Harbor pull)
//! 2. Check if packet_id already seen for this topic
//! 3. If new: mark as seen, then process
//! 4. If seen: skip processing
//!
//! # Retention
//!
//! Seen packets are tracked for `PACKET_LIFETIME_SECS` (90 days) - aligned
//! with Harbor packet lifetime. As long as a packet could exist on Harbor,
//! we remember if we've seen it.

use rusqlite::{Connection, params};
use tracing::trace;

use crate::security::PacketId;

/// Result of deduplication check
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupResult {
    /// Packet should be processed (first time seeing it)
    Process,
    /// Packet is our own (skip)
    OwnPacket,
    /// Packet was already seen (skip)
    AlreadySeen,
}

impl DedupResult {
    /// Check if the packet should be processed
    pub fn should_process(&self) -> bool {
        matches!(self, DedupResult::Process)
    }
}

impl std::fmt::Display for DedupResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DedupResult::Process => write!(f, "process"),
            DedupResult::OwnPacket => write!(f, "own packet"),
            DedupResult::AlreadySeen => write!(f, "already seen"),
        }
    }
}

/// Check if a packet should be processed and mark it as seen
///
/// This is the main deduplication entry point. It:
/// 1. Checks if the sender is us (skip own packets)
/// 2. Checks if we've already seen this packet
/// 3. Marks the packet as seen if it's new
///
/// # Arguments
/// * `conn` - Database connection
/// * `topic_id` - Topic the packet belongs to
/// * `packet_id` - Unique packet identifier
/// * `sender_id` - Sender's endpoint ID
/// * `our_id` - Our endpoint ID
pub fn check_and_mark(
    conn: &Connection,
    topic_id: &[u8; 32],
    packet_id: &PacketId,
    sender_id: &[u8; 32],
    our_id: &[u8; 32],
) -> Result<DedupResult, DedupError> {
    // Rule 1: Never process our own packets
    if sender_id == our_id {
        trace!(
            topic_id = %hex::encode(&topic_id[..8]),
            packet_id = %hex::encode(&packet_id[..8]),
            result = "OwnPacket",
            "DEDUP: skipping own packet"
        );
        return Ok(DedupResult::OwnPacket);
    }

    // Rule 2+3 atomically: mark as seen if new, otherwise detect duplicate.
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO pulled_packets (topic_id, packet_id, pulled_at)
             VALUES (?1, ?2, ?3)",
            params![
                topic_id.as_slice(),
                packet_id.as_slice(),
                crate::data::current_timestamp()
            ],
        )
        .map_err(|e| DedupError::Database(e.to_string()))?;

    if inserted == 0 {
        trace!(
            topic_id = %hex::encode(&topic_id[..8]),
            packet_id = %hex::encode(&packet_id[..8]),
            result = "AlreadySeen",
            "DEDUP: packet already seen"
        );
        return Ok(DedupResult::AlreadySeen);
    }

    trace!(
        topic_id = %hex::encode(&topic_id[..8]),
        packet_id = %hex::encode(&packet_id[..8]),
        result = "Process",
        "DEDUP: new packet, marked as seen"
    );

    Ok(DedupResult::Process)
}

/// Get count of tracked packets for a topic
pub fn get_tracked_count(conn: &Connection, topic_id: &[u8; 32]) -> Result<usize, DedupError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pulled_packets WHERE topic_id = ?1",
            [topic_id.as_slice()],
            |row| row.get(0),
        )
        .map_err(|e| DedupError::Database(e.to_string()))?;

    Ok(count as usize)
}

/// Get total count of tracked packets (all topics)
pub fn get_total_tracked_count(conn: &Connection) -> Result<usize, DedupError> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pulled_packets", [], |row| row.get(0))
        .map_err(|e| DedupError::Database(e.to_string()))?;

    Ok(count as usize)
}

/// Deduplication errors
#[derive(Debug, Clone)]
pub enum DedupError {
    Database(String),
}

impl std::fmt::Display for DedupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DedupError::Database(e) => write!(f, "database error: {}", e),
        }
    }
}

impl std::error::Error for DedupError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::current_timestamp;
    use crate::data::harbor::cache::{
        PACKET_LIFETIME_SECS, cleanup_old_pulled_packets, mark_pulled,
    };
    use crate::data::schema::create_harbor_table;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn test_packet_id(seed: u8) -> PacketId {
        [seed; 16]
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    // ===== DedupResult tests =====

    #[test]
    fn test_dedup_result_should_process() {
        assert!(DedupResult::Process.should_process());
        assert!(!DedupResult::OwnPacket.should_process());
        assert!(!DedupResult::AlreadySeen.should_process());
    }

    #[test]
    fn test_dedup_result_display() {
        assert_eq!(DedupResult::Process.to_string(), "process");
        assert_eq!(DedupResult::OwnPacket.to_string(), "own packet");
        assert_eq!(DedupResult::AlreadySeen.to_string(), "already seen");
    }

    // ===== check_and_mark tests =====

    #[test]
    fn test_check_and_mark_new_packet() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let packet_id = test_packet_id(10);
        let sender_id = test_id(20);
        let our_id = test_id(99);

        let result = check_and_mark(&conn, &topic_id, &packet_id, &sender_id, &our_id).unwrap();
        assert_eq!(result, DedupResult::Process);
    }

    #[test]
    fn test_check_and_mark_own_packet() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let packet_id = test_packet_id(10);
        let our_id = test_id(99);

        // Sender is us
        let result = check_and_mark(&conn, &topic_id, &packet_id, &our_id, &our_id).unwrap();
        assert_eq!(result, DedupResult::OwnPacket);
    }

    #[test]
    fn test_check_and_mark_already_seen() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let packet_id = test_packet_id(10);
        let sender_id = test_id(20);
        let our_id = test_id(99);

        // First time - should process
        let result1 = check_and_mark(&conn, &topic_id, &packet_id, &sender_id, &our_id).unwrap();
        assert_eq!(result1, DedupResult::Process);

        // Second time - already seen
        let result2 = check_and_mark(&conn, &topic_id, &packet_id, &sender_id, &our_id).unwrap();
        assert_eq!(result2, DedupResult::AlreadySeen);
    }

    #[test]
    fn test_check_and_mark_pre_marked_packet() {
        let conn = setup_db();
        let topic_id = test_id(9);
        let packet_id = test_packet_id(33);
        let sender_id = test_id(20);
        let our_id = test_id(99);

        mark_pulled(&conn, &topic_id, &packet_id).unwrap();
        let result = check_and_mark(&conn, &topic_id, &packet_id, &sender_id, &our_id).unwrap();
        assert_eq!(result, DedupResult::AlreadySeen);
    }

    #[test]
    fn test_check_and_mark_different_packets() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let our_id = test_id(99);

        let result1 =
            check_and_mark(&conn, &topic_id, &test_packet_id(10), &test_id(20), &our_id).unwrap();
        let result2 =
            check_and_mark(&conn, &topic_id, &test_packet_id(11), &test_id(20), &our_id).unwrap();

        // Both should process (different packet IDs)
        assert_eq!(result1, DedupResult::Process);
        assert_eq!(result2, DedupResult::Process);
    }

    #[test]
    fn test_check_and_mark_different_topics() {
        let conn = setup_db();
        let packet_id = test_packet_id(10);
        let sender_id = test_id(20);
        let our_id = test_id(99);

        let result1 = check_and_mark(&conn, &test_id(1), &packet_id, &sender_id, &our_id).unwrap();
        let result2 = check_and_mark(&conn, &test_id(2), &packet_id, &sender_id, &our_id).unwrap();

        // Both should process (different topics)
        assert_eq!(result1, DedupResult::Process);
        assert_eq!(result2, DedupResult::Process);
    }

    // ===== count tests =====

    #[test]
    fn test_get_tracked_count() {
        let conn = setup_db();
        let topic_id = test_id(1);

        assert_eq!(get_tracked_count(&conn, &topic_id).unwrap(), 0);

        mark_pulled(&conn, &topic_id, &test_packet_id(10)).unwrap();
        assert_eq!(get_tracked_count(&conn, &topic_id).unwrap(), 1);

        mark_pulled(&conn, &topic_id, &test_packet_id(11)).unwrap();
        assert_eq!(get_tracked_count(&conn, &topic_id).unwrap(), 2);
    }

    #[test]
    fn test_get_tracked_count_per_topic() {
        let conn = setup_db();

        mark_pulled(&conn, &test_id(1), &test_packet_id(10)).unwrap();
        mark_pulled(&conn, &test_id(1), &test_packet_id(11)).unwrap();
        mark_pulled(&conn, &test_id(2), &test_packet_id(20)).unwrap();

        assert_eq!(get_tracked_count(&conn, &test_id(1)).unwrap(), 2);
        assert_eq!(get_tracked_count(&conn, &test_id(2)).unwrap(), 1);
        assert_eq!(get_tracked_count(&conn, &test_id(3)).unwrap(), 0);
    }

    #[test]
    fn test_get_total_tracked_count() {
        let conn = setup_db();

        assert_eq!(get_total_tracked_count(&conn).unwrap(), 0);

        mark_pulled(&conn, &test_id(1), &test_packet_id(10)).unwrap();
        mark_pulled(&conn, &test_id(2), &test_packet_id(20)).unwrap();
        mark_pulled(&conn, &test_id(3), &test_packet_id(30)).unwrap();

        assert_eq!(get_total_tracked_count(&conn).unwrap(), 3);
    }

    // ===== cleanup tests =====

    #[test]
    fn test_cleanup_removes_old_packets() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let packet_id = test_packet_id(10);

        // Insert with old timestamp
        let old_timestamp = current_timestamp() - PACKET_LIFETIME_SECS - 1;
        conn.execute(
            "INSERT INTO pulled_packets (topic_id, packet_id, pulled_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![topic_id.as_slice(), packet_id.as_slice(), old_timestamp],
        )
        .unwrap();

        // Verify it exists
        assert_eq!(get_tracked_count(&conn, &topic_id).unwrap(), 1);

        // Cleanup should remove it
        let deleted = cleanup_old_pulled_packets(&conn).unwrap();
        assert_eq!(deleted, 1);

        // Gone
        assert_eq!(get_tracked_count(&conn, &topic_id).unwrap(), 0);
    }

    // ===== error display tests =====

    #[test]
    fn test_dedup_error_display() {
        let err = DedupError::Database("test error".to_string());
        assert!(err.to_string().contains("database"));
        assert!(err.to_string().contains("test error"));
    }
}
