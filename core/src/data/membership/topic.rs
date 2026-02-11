//! Topic subscription management
//!
//! Handles storage and retrieval of topic subscriptions and their members.
//! Topics are identified by a 32-byte TopicID.

use rusqlite::{Connection, OptionalExtension, params};

use crate::data::dht::{current_timestamp, ensure_peer_exists};
use crate::data::membership::epoch::store_epoch_key;
use crate::security::topic_keys::{TopicKeys, harbor_id_from_topic};
use crate::security::derive_epoch_secret_from_topic;

/// A topic subscription stored in the database
#[derive(Clone)]
pub struct TopicSubscription {
    /// 32-byte TopicID
    pub topic_id: [u8; 32],
    /// 32-byte HarborID (hash of TopicID)
    pub harbor_id: [u8; 32],
    /// Derived encryption key
    pub k_enc: [u8; 32],
    /// Derived MAC key
    pub k_mac: [u8; 32],
    /// When we subscribed
    pub created_at: i64,
}

// Custom Debug to avoid exposing keys in logs
impl std::fmt::Debug for TopicSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopicSubscription")
            .field("topic_id", &hex::encode(self.topic_id))
            .field("harbor_id", &hex::encode(self.harbor_id))
            .field("k_enc", &"[REDACTED]")
            .field("k_mac", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl TopicSubscription {
    /// Create a new subscription from a TopicID
    pub fn new(topic_id: [u8; 32]) -> Self {
        let harbor_id = harbor_id_from_topic(&topic_id);
        let keys = TopicKeys::derive(&topic_id);
        
        Self {
            topic_id,
            harbor_id,
            k_enc: keys.k_enc,
            k_mac: keys.k_mac,
            created_at: current_timestamp(),
        }
    }

    /// Get the TopicKeys
    pub fn keys(&self) -> TopicKeys {
        TopicKeys {
            k_enc: self.k_enc,
            k_mac: self.k_mac,
        }
    }
}

/// A topic member
#[derive(Debug, Clone)]
pub struct TopicMember {
    /// Topic this member belongs to
    pub topic_id: [u8; 32],
    /// Member's EndpointID
    pub endpoint_id: [u8; 32],
    /// When they joined
    pub joined_at: i64,
}

/// Subscribe to a topic (for joining an existing topic via invite)
///
/// Creates the topic subscription with the sender as admin.
/// Uses INSERT OR IGNORE to preserve the original `created_at` timestamp
/// if the topic already exists.
///
/// For creating new topics where you are the admin, use `subscribe_topic_with_admin`.
pub fn subscribe_topic(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<TopicSubscription> {
    // For backwards compatibility, use a placeholder admin
    // This will be overwritten when proper invite flow sets the real admin
    let placeholder_admin = [0u8; 32];
    subscribe_topic_with_admin(conn, topic_id, &placeholder_admin)
}

/// Subscribe to a topic with explicit admin
///
/// Creates the topic subscription, stores the admin_id, and stores epoch 0 key.
/// Does NOT add any members - use `add_topic_member` for that.
///
/// Uses INSERT OR IGNORE to preserve the original `created_at` timestamp
/// if the topic already exists. This is important for Harbor pull filtering -
/// we want to receive packets sent after our ORIGINAL join time, not after
/// a rejoin/restart.
///
/// Also stores epoch 0 key (derived from topic_id) so all topics start with
/// an epoch key. This simplifies the encryption flow - no special case for
/// "no epoch key yet" since every topic has at least epoch 0.
pub fn subscribe_topic_with_admin(
    conn: &Connection,
    topic_id: &[u8; 32],
    admin_id: &[u8; 32],
) -> rusqlite::Result<TopicSubscription> {
    let subscription = TopicSubscription::new(*topic_id);

    // Ensure admin exists in peers table (for FK constraint)
    ensure_peer_exists(conn, admin_id)?;

    // Use INSERT OR IGNORE to preserve original created_at on rejoin
    conn.execute(
        "INSERT OR IGNORE INTO topics (topic_id, harbor_id, admin_peer_id)
         VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
        params![topic_id.as_slice(), subscription.harbor_id.as_slice(), admin_id.as_slice()],
    )?;

    // Store epoch 0 key derived from topic_id
    // This ensures all topics have at least one epoch key from the start
    let epoch_0_secret = derive_epoch_secret_from_topic(topic_id, 0);
    store_epoch_key(conn, topic_id, 0, &epoch_0_secret)?;

    // Return the actual subscription from DB (may have older created_at)
    get_topic(conn, topic_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// Subscribe to a DM "topic" for unified harbor pull handling
///
/// DMs use the raw endpoint_id as both topic_id and harbor_id (NOT hashed).
/// This is different from regular topics which hash the topic_id to create harbor_id.
///
/// This allows the harbor pull loop to handle DMs and regular topics identically:
/// - Both are stored in the topics table
/// - Both get harbor node discovery via the standard discovery task
/// - The pull loop detects DMs by checking if topic_id == our_id
///
/// Note: DMs don't use epoch keys (they use DM shared keys), so we don't store
/// an epoch key here.
pub fn subscribe_dm_topic(
    conn: &Connection,
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<()> {
    // Ensure endpoint exists in peers table (for FK constraint)
    ensure_peer_exists(conn, endpoint_id)?;

    // For DMs, harbor_id = endpoint_id (raw, NOT hashed)
    // This matches how DM packets are stored in Harbor (harbor_id = recipient's endpoint_id)
    conn.execute(
        "INSERT OR IGNORE INTO topics (topic_id, harbor_id, admin_peer_id)
         VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
        params![endpoint_id.as_slice(), endpoint_id.as_slice(), endpoint_id.as_slice()],
    )?;

    Ok(())
}

/// Unsubscribe from a topic
///
/// Removes the topic and all members (cascade delete).
pub fn unsubscribe_topic(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM topics WHERE topic_id = ?1",
        [topic_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Check if subscribed to a topic
pub fn is_subscribed(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topics WHERE topic_id = ?1",
        [topic_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get the admin ID for a topic
pub fn get_topic_admin(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<Option<[u8; 32]>> {
    conn.query_row(
        "SELECT p.endpoint_id
         FROM topics t
         JOIN peers p ON t.admin_peer_id = p.id
         WHERE t.topic_id = ?1",
        [topic_id.as_slice()],
        |row| {
            let admin_vec: Vec<u8> = row.get(0)?;
            if admin_vec.len() != 32 {
                return Err(rusqlite::Error::InvalidColumnType(
                    0,
                    "admin_id".to_string(),
                    rusqlite::types::Type::Blob,
                ));
            }
            let mut admin_id = [0u8; 32];
            admin_id.copy_from_slice(&admin_vec);
            Ok(admin_id)
        },
    )
    .optional()
}

/// Check if a peer is the admin of a topic
pub fn is_topic_admin(
    conn: &Connection,
    topic_id: &[u8; 32],
    peer_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topics
         WHERE topic_id = ?1
           AND admin_peer_id = (SELECT id FROM peers WHERE endpoint_id = ?2)",
        params![topic_id.as_slice(), peer_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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

/// Parse a TopicSubscription from a database row
fn parse_topic_row(row: &rusqlite::Row) -> rusqlite::Result<TopicSubscription> {
    let topic_id_vec: Vec<u8> = row.get(0)?;
    let harbor_id_vec: Vec<u8> = row.get(1)?;
    
    let topic_id = parse_id(&topic_id_vec, "topic_id")?;
    let harbor_id = parse_id(&harbor_id_vec, "harbor_id")?;
    
    // Derive keys from topic_id
    let keys = TopicKeys::derive(&topic_id);
    
    Ok(TopicSubscription {
        topic_id,
        harbor_id,
        k_enc: keys.k_enc,
        k_mac: keys.k_mac,
        created_at: row.get(2)?,
    })
}

/// Get a topic subscription
pub fn get_topic(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<Option<TopicSubscription>> {
    conn.query_row(
        "SELECT topic_id, harbor_id, created_at FROM topics WHERE topic_id = ?1",
        [topic_id.as_slice()],
        parse_topic_row,
    )
    .optional()
}

/// Get topic by HarborID
pub fn get_topic_by_harbor_id(conn: &Connection, harbor_id: &[u8; 32]) -> rusqlite::Result<Option<TopicSubscription>> {
    conn.query_row(
        "SELECT topic_id, harbor_id, created_at FROM topics WHERE harbor_id = ?1",
        [harbor_id.as_slice()],
        parse_topic_row,
    )
    .optional()
}

/// Get all subscribed topics
pub fn get_all_topics(conn: &Connection) -> rusqlite::Result<Vec<TopicSubscription>> {
    let mut stmt = conn.prepare(
        "SELECT topic_id, harbor_id, created_at FROM topics ORDER BY created_at DESC",
    )?;
    
    let topics = stmt
        .query_map([], parse_topic_row)?
        .collect::<Result<Vec<_>, _>>()?;
    
    Ok(topics)
}

// ============ Member Management ============

/// Add a member to a topic
pub fn add_topic_member(
    conn: &Connection,
    topic_id: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<()> {
    // Ensure member exists in peers table (for FK constraint)
    ensure_peer_exists(conn, endpoint_id)?;

    conn.execute(
        "INSERT OR REPLACE INTO topic_members (topic_id, peer_id)
         VALUES (?1, (SELECT id FROM peers WHERE endpoint_id = ?2))",
        params![topic_id.as_slice(), endpoint_id.as_slice()],
    )?;
    Ok(())
}

/// Remove a member from a topic
pub fn remove_topic_member(
    conn: &Connection,
    topic_id: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM topic_members
         WHERE topic_id = ?1
           AND peer_id = (SELECT id FROM peers WHERE endpoint_id = ?2)",
        params![topic_id.as_slice(), endpoint_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Check if an endpoint is a member of a topic
pub fn is_topic_member(
    conn: &Connection,
    topic_id: &[u8; 32],
    endpoint_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topic_members
         WHERE topic_id = ?1
           AND peer_id = (SELECT id FROM peers WHERE endpoint_id = ?2)",
        params![topic_id.as_slice(), endpoint_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Get all members of a topic (endpoint IDs only)
pub fn get_topic_members(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id
         FROM topic_members tm
         JOIN peers p ON tm.peer_id = p.id
         WHERE tm.topic_id = ?1
         ORDER BY tm.joined_at",
    )?;
    
    let members = stmt
        .query_map([topic_id.as_slice()], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "endpoint_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;
    
    Ok(members)
}

/// Get member count for a topic
pub fn get_topic_member_count(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topic_members WHERE topic_id = ?1",
        [topic_id.as_slice()],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Set all members for a topic (replaces existing)
///
/// This operation is atomic - either all members are replaced or none are.
pub fn set_topic_members(
    conn: &mut Connection,
    topic_id: &[u8; 32],
    members: &[[u8; 32]],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;

    // Remove existing members
    tx.execute(
        "DELETE FROM topic_members WHERE topic_id = ?1",
        [topic_id.as_slice()],
    )?;

    // Add new members
    for member in members {
        // Ensure member exists in peers table (for FK constraint)
        ensure_peer_exists(&tx, member)?;
        tx.execute(
            "INSERT OR REPLACE INTO topic_members (topic_id, peer_id)
             VALUES (?1, (SELECT id FROM peers WHERE endpoint_id = ?2))",
            params![topic_id.as_slice(), member.as_slice()],
        )?;
    }

    tx.commit()
}

/// Get topics that a specific endpoint is a member of
pub fn get_topics_for_member(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT tm.topic_id
         FROM topic_members tm
         JOIN peers p ON tm.peer_id = p.id
         WHERE p.endpoint_id = ?1",
    )?;
    
    let topics = stmt
        .query_map([endpoint_id.as_slice()], |row| {
            let id_vec: Vec<u8> = row.get(0)?;
            parse_id(&id_vec, "topic_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;
    
    Ok(topics)
}

// ============ Topic Join Timestamp ============

/// Get the timestamp when we joined (subscribed to) a topic
///
/// Returns the created_at timestamp from topics table, or 0 if not found.
/// This is used by Harbor pull to filter out messages sent before we joined.
pub fn get_joined_at(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT created_at FROM topics WHERE topic_id = ?1",
        [topic_id.as_slice()],
        |row| row.get(0),
    )
    .optional()
    .map(|opt| opt.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_topic_table, create_peer_table, create_membership_tables};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();
        create_membership_tables(&conn).unwrap();
        conn
    }

    fn test_topic_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn test_endpoint_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_subscribe_topic() {
        let conn = setup_db();
        let topic_id = test_topic_id(1);
        
        let subscription = subscribe_topic(&conn, &topic_id).unwrap();
        
        assert_eq!(subscription.topic_id, topic_id);
        assert_eq!(subscription.harbor_id, harbor_id_from_topic(&topic_id));
        assert!(is_subscribed(&conn, &topic_id).unwrap());
    }

    #[test]
    fn test_unsubscribe_topic() {
        let conn = setup_db();
        let topic_id = test_topic_id(2);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        assert!(is_subscribed(&conn, &topic_id).unwrap());
        
        unsubscribe_topic(&conn, &topic_id).unwrap();
        assert!(!is_subscribed(&conn, &topic_id).unwrap());
    }

    #[test]
    fn test_get_topic() {
        let conn = setup_db();
        let topic_id = test_topic_id(3);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        
        let topic = get_topic(&conn, &topic_id).unwrap().unwrap();
        assert_eq!(topic.topic_id, topic_id);
        
        // Keys should be derivable
        let keys = topic.keys();
        let expected = TopicKeys::derive(&topic_id);
        assert_eq!(keys.k_enc, expected.k_enc);
        assert_eq!(keys.k_mac, expected.k_mac);
    }

    #[test]
    fn test_get_topic_by_harbor_id() {
        let conn = setup_db();
        let topic_id = test_topic_id(4);
        let harbor_id = harbor_id_from_topic(&topic_id);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        
        let topic = get_topic_by_harbor_id(&conn, &harbor_id).unwrap().unwrap();
        assert_eq!(topic.topic_id, topic_id);
    }

    #[test]
    fn test_add_topic_member() {
        let conn = setup_db();
        let topic_id = test_topic_id(5);
        let member = test_endpoint_id(10);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &member).unwrap();
        
        assert!(is_topic_member(&conn, &topic_id, &member).unwrap());
    }

    #[test]
    fn test_remove_topic_member() {
        let conn = setup_db();
        let topic_id = test_topic_id(6);
        let member = test_endpoint_id(20);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &member).unwrap();
        assert!(is_topic_member(&conn, &topic_id, &member).unwrap());
        
        remove_topic_member(&conn, &topic_id, &member).unwrap();
        assert!(!is_topic_member(&conn, &topic_id, &member).unwrap());
    }

    #[test]
    fn test_get_topic_members() {
        let conn = setup_db();
        let topic_id = test_topic_id(7);
        let members = [test_endpoint_id(30), test_endpoint_id(31), test_endpoint_id(32)];
        
        subscribe_topic(&conn, &topic_id).unwrap();
        for member in &members {
            add_topic_member(&conn, &topic_id, member).unwrap();
        }
        
        let retrieved = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(retrieved.len(), 3);
        for member in &members {
            assert!(retrieved.contains(member));
        }
    }

    #[test]
    fn test_set_topic_members() {
        let mut conn = setup_db();
        let topic_id = test_topic_id(8);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        
        // Set initial members
        let members1 = [test_endpoint_id(40), test_endpoint_id(41)];
        set_topic_members(&mut conn, &topic_id, &members1).unwrap();
        assert_eq!(get_topic_member_count(&conn, &topic_id).unwrap(), 2);
        
        // Replace with new members
        let members2 = [test_endpoint_id(50), test_endpoint_id(51), test_endpoint_id(52)];
        set_topic_members(&mut conn, &topic_id, &members2).unwrap();
        assert_eq!(get_topic_member_count(&conn, &topic_id).unwrap(), 3);
        
        // Old members should be gone
        assert!(!is_topic_member(&conn, &topic_id, &members1[0]).unwrap());
    }

    #[test]
    fn test_unsubscribe_removes_members() {
        let conn = setup_db();
        let topic_id = test_topic_id(9);
        
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &test_endpoint_id(60)).unwrap();
        add_topic_member(&conn, &topic_id, &test_endpoint_id(61)).unwrap();
        
        assert_eq!(get_topic_member_count(&conn, &topic_id).unwrap(), 2);
        
        unsubscribe_topic(&conn, &topic_id).unwrap();
        
        // Members should be cascade deleted
        assert_eq!(get_topic_member_count(&conn, &topic_id).unwrap(), 0);
    }

    #[test]
    fn test_get_all_topics() {
        let conn = setup_db();
        
        subscribe_topic(&conn, &test_topic_id(10)).unwrap();
        subscribe_topic(&conn, &test_topic_id(11)).unwrap();
        subscribe_topic(&conn, &test_topic_id(12)).unwrap();
        
        let topics = get_all_topics(&conn).unwrap();
        assert_eq!(topics.len(), 3);
    }

    #[test]
    fn test_get_topics_for_member() {
        let conn = setup_db();
        let member = test_endpoint_id(70);
        
        // Member in topics 20 and 21, not in 22
        subscribe_topic(&conn, &test_topic_id(20)).unwrap();
        subscribe_topic(&conn, &test_topic_id(21)).unwrap();
        subscribe_topic(&conn, &test_topic_id(22)).unwrap();
        
        add_topic_member(&conn, &test_topic_id(20), &member).unwrap();
        add_topic_member(&conn, &test_topic_id(21), &member).unwrap();
        
        let topics = get_topics_for_member(&conn, &member).unwrap();
        assert_eq!(topics.len(), 2);
        assert!(topics.contains(&test_topic_id(20)));
        assert!(topics.contains(&test_topic_id(21)));
        assert!(!topics.contains(&test_topic_id(22)));
    }

    #[test]
    fn test_topic_subscription_new() {
        let topic_id = test_topic_id(100);
        let sub = TopicSubscription::new(topic_id);
        
        assert_eq!(sub.topic_id, topic_id);
        assert_eq!(sub.harbor_id, harbor_id_from_topic(&topic_id));
        
        let keys = sub.keys();
        let expected = TopicKeys::derive(&topic_id);
        assert_eq!(keys.k_enc, expected.k_enc);
        assert_eq!(keys.k_mac, expected.k_mac);
    }

    #[test]
    fn test_debug_does_not_expose_keys() {
        let topic_id = test_topic_id(101);
        let sub = TopicSubscription::new(topic_id);
        
        let debug_output = format!("{:?}", sub);
        
        // Should contain REDACTED for keys
        assert!(debug_output.contains("[REDACTED]"), "Debug should redact keys");
        
        // Should show topic_id in hex
        let topic_hex = hex::encode(topic_id);
        assert!(debug_output.contains(&topic_hex), "Debug should show topic_id");
        
        // Should NOT contain actual key bytes
        let k_enc_hex = hex::encode(sub.k_enc);
        assert!(!debug_output.contains(&k_enc_hex), "Debug should NOT contain k_enc");
    }
}
