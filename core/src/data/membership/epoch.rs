//! Epoch key management
//!
//! Handles storage and retrieval of epoch keys for topics.
//! Each topic can have multiple epoch keys (for key rotation on member removal).

use rusqlite::{Connection, OptionalExtension, params};

use crate::data::dht::current_timestamp;

/// Default epoch key lifetime: 90 days
pub const EPOCH_KEY_LIFETIME_SECS: i64 = 90 * 24 * 60 * 60;

/// An epoch key for a topic
#[derive(Clone)]
pub struct EpochKey {
    pub topic_id: [u8; 32],
    pub epoch: u64,
    pub key_data: [u8; 32],
    pub received_at: i64,
    pub expires_at: i64,
}

// Custom Debug to avoid exposing keys in logs
impl std::fmt::Debug for EpochKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpochKey")
            .field("topic_id", &hex::encode(&self.topic_id[..8]))
            .field("epoch", &self.epoch)
            .field("key_data", &"[REDACTED]")
            .field("received_at", &self.received_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Store an epoch key for a topic
///
/// Replaces any existing key for the same topic+epoch.
pub fn store_epoch_key(
    conn: &Connection,
    topic_id: &[u8; 32],
    epoch: u64,
    key_data: &[u8; 32],
) -> rusqlite::Result<()> {
    let now = current_timestamp();
    let expires_at = now + EPOCH_KEY_LIFETIME_SECS;

    conn.execute(
        "INSERT OR REPLACE INTO epoch_keys (topic_id, epoch, key_data, received_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            topic_id.as_slice(),
            epoch as i64,
            key_data.as_slice(),
            now,
            expires_at,
        ],
    )?;
    Ok(())
}

/// Get an epoch key for a specific epoch
pub fn get_epoch_key(
    conn: &Connection,
    topic_id: &[u8; 32],
    epoch: u64,
) -> rusqlite::Result<Option<EpochKey>> {
    conn.query_row(
        "SELECT topic_id, epoch, key_data, received_at, expires_at
         FROM epoch_keys WHERE topic_id = ?1 AND epoch = ?2",
        params![topic_id.as_slice(), epoch as i64],
        |row| {
            let topic_id_vec: Vec<u8> = row.get(0)?;
            let key_data_vec: Vec<u8> = row.get(2)?;

            let mut topic_id = [0u8; 32];
            let mut key_data = [0u8; 32];
            topic_id.copy_from_slice(&topic_id_vec);
            key_data.copy_from_slice(&key_data_vec);

            Ok(EpochKey {
                topic_id,
                epoch: row.get::<_, i64>(1)? as u64,
                key_data,
                received_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        },
    )
    .optional()
}

/// Get the current (highest) epoch for a topic
pub fn get_current_epoch(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<Option<u64>> {
    conn.query_row(
        "SELECT MAX(epoch) FROM epoch_keys WHERE topic_id = ?1",
        [topic_id.as_slice()],
        |row| {
            let epoch: Option<i64> = row.get(0)?;
            Ok(epoch.map(|e| e as u64))
        },
    )
}

/// Get the current epoch key (highest epoch) for a topic
pub fn get_current_epoch_key(
    conn: &Connection,
    topic_id: &[u8; 32],
) -> rusqlite::Result<Option<EpochKey>> {
    conn.query_row(
        "SELECT topic_id, epoch, key_data, received_at, expires_at
         FROM epoch_keys WHERE topic_id = ?1
         ORDER BY epoch DESC LIMIT 1",
        [topic_id.as_slice()],
        |row| {
            let topic_id_vec: Vec<u8> = row.get(0)?;
            let key_data_vec: Vec<u8> = row.get(2)?;

            let mut topic_id = [0u8; 32];
            let mut key_data = [0u8; 32];
            topic_id.copy_from_slice(&topic_id_vec);
            key_data.copy_from_slice(&key_data_vec);

            Ok(EpochKey {
                topic_id,
                epoch: row.get::<_, i64>(1)? as u64,
                key_data,
                received_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        },
    )
    .optional()
}

/// Get all epoch keys for a topic (oldest to newest)
pub fn get_all_epoch_keys(
    conn: &Connection,
    topic_id: &[u8; 32],
) -> rusqlite::Result<Vec<EpochKey>> {
    let mut stmt = conn.prepare(
        "SELECT topic_id, epoch, key_data, received_at, expires_at
         FROM epoch_keys WHERE topic_id = ?1
         ORDER BY epoch ASC",
    )?;

    let keys = stmt
        .query_map([topic_id.as_slice()], |row| {
            let topic_id_vec: Vec<u8> = row.get(0)?;
            let key_data_vec: Vec<u8> = row.get(2)?;

            let mut topic_id = [0u8; 32];
            let mut key_data = [0u8; 32];
            topic_id.copy_from_slice(&topic_id_vec);
            key_data.copy_from_slice(&key_data_vec);

            Ok(EpochKey {
                topic_id,
                epoch: row.get::<_, i64>(1)? as u64,
                key_data,
                received_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(keys)
}

/// Delete expired epoch keys
pub fn cleanup_expired_epoch_keys(conn: &Connection) -> rusqlite::Result<usize> {
    let now = current_timestamp();
    let rows = conn.execute(
        "DELETE FROM epoch_keys WHERE expires_at < ?1",
        [now],
    )?;
    Ok(rows)
}

/// Delete all epoch keys for a topic
pub fn delete_epoch_keys_for_topic(conn: &Connection, topic_id: &[u8; 32]) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM epoch_keys WHERE topic_id = ?1",
        [topic_id.as_slice()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_peer_table, create_topic_table, create_membership_tables};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();
        create_membership_tables(&conn).unwrap();
        conn
    }

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    /// Helper to create a topic (required for FK constraint on epoch_keys)
    fn create_test_topic(conn: &Connection, topic_id: &[u8; 32]) {
        let admin_id = [0xADu8; 32];
        let harbor_id = [0xBBu8; 32];
        // Insert admin peer first
        conn.execute(
            "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![admin_id.as_slice(), 0i64],
        ).unwrap();
        // Insert topic
        conn.execute(
            "INSERT OR IGNORE INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            rusqlite::params![topic_id.as_slice(), harbor_id.as_slice(), admin_id.as_slice()],
        )
        .unwrap();
    }

    #[test]
    fn test_store_and_get_epoch_key() {
        let conn = setup_db();
        let topic_id = test_id(1);
        let key_data = test_id(100);

        create_test_topic(&conn, &topic_id);
        store_epoch_key(&conn, &topic_id, 1, &key_data).unwrap();

        let key = get_epoch_key(&conn, &topic_id, 1).unwrap().unwrap();
        assert_eq!(key.topic_id, topic_id);
        assert_eq!(key.epoch, 1);
        assert_eq!(key.key_data, key_data);
    }

    #[test]
    fn test_get_current_epoch() {
        let conn = setup_db();
        let topic_id = test_id(2);

        create_test_topic(&conn, &topic_id);

        // No epochs yet
        assert!(get_current_epoch(&conn, &topic_id).unwrap().is_none());

        // Add epochs
        store_epoch_key(&conn, &topic_id, 1, &test_id(101)).unwrap();
        store_epoch_key(&conn, &topic_id, 3, &test_id(103)).unwrap();
        store_epoch_key(&conn, &topic_id, 2, &test_id(102)).unwrap();

        let current = get_current_epoch(&conn, &topic_id).unwrap().unwrap();
        assert_eq!(current, 3);
    }

    #[test]
    fn test_get_current_epoch_key() {
        let conn = setup_db();
        let topic_id = test_id(3);
        let key_data = test_id(105);

        create_test_topic(&conn, &topic_id);

        store_epoch_key(&conn, &topic_id, 1, &test_id(101)).unwrap();
        store_epoch_key(&conn, &topic_id, 5, &key_data).unwrap();
        store_epoch_key(&conn, &topic_id, 3, &test_id(103)).unwrap();

        let key = get_current_epoch_key(&conn, &topic_id).unwrap().unwrap();
        assert_eq!(key.epoch, 5);
        assert_eq!(key.key_data, key_data);
    }

    #[test]
    fn test_get_all_epoch_keys() {
        let conn = setup_db();
        let topic_id = test_id(4);

        create_test_topic(&conn, &topic_id);

        store_epoch_key(&conn, &topic_id, 3, &test_id(103)).unwrap();
        store_epoch_key(&conn, &topic_id, 1, &test_id(101)).unwrap();
        store_epoch_key(&conn, &topic_id, 2, &test_id(102)).unwrap();

        let keys = get_all_epoch_keys(&conn, &topic_id).unwrap();
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].epoch, 1);
        assert_eq!(keys[1].epoch, 2);
        assert_eq!(keys[2].epoch, 3);
    }

    #[test]
    fn test_store_replaces_existing() {
        let conn = setup_db();
        let topic_id = test_id(5);
        let key_data1 = test_id(110);
        let key_data2 = test_id(120);

        create_test_topic(&conn, &topic_id);

        store_epoch_key(&conn, &topic_id, 1, &key_data1).unwrap();
        store_epoch_key(&conn, &topic_id, 1, &key_data2).unwrap();

        let key = get_epoch_key(&conn, &topic_id, 1).unwrap().unwrap();
        assert_eq!(key.key_data, key_data2);

        // Should only be one key
        let keys = get_all_epoch_keys(&conn, &topic_id).unwrap();
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn test_delete_epoch_keys_for_topic() {
        let conn = setup_db();
        let topic_id = test_id(6);

        create_test_topic(&conn, &topic_id);

        store_epoch_key(&conn, &topic_id, 1, &test_id(111)).unwrap();
        store_epoch_key(&conn, &topic_id, 2, &test_id(112)).unwrap();

        delete_epoch_keys_for_topic(&conn, &topic_id).unwrap();

        assert!(get_current_epoch(&conn, &topic_id).unwrap().is_none());
    }
}
