//! Epoch key management
//!
//! Handles storage and retrieval of epoch keys for topics.
//! Each topic can have multiple epoch keys (for key rotation on member removal).

use rusqlite::types::Type;
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

fn invalid_blob_column(index: usize, name: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(index, name.to_string(), Type::Blob)
}

fn invalid_integer_column(index: usize, name: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(index, name.to_string(), Type::Integer)
}

fn decode_fixed_32(bytes: Vec<u8>, index: usize, name: &'static str) -> rusqlite::Result<[u8; 32]> {
    if bytes.len() != 32 {
        return Err(invalid_blob_column(index, name));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decode_non_negative_epoch(epoch: i64, index: usize) -> rusqlite::Result<u64> {
    if epoch < 0 {
        return Err(invalid_integer_column(index, "epoch"));
    }
    Ok(epoch as u64)
}

fn encode_epoch_param(epoch: u64) -> rusqlite::Result<i64> {
    i64::try_from(epoch)
        .map_err(|_| rusqlite::Error::InvalidParameterName("epoch out of i64 range".to_string()))
}

fn parse_epoch_key_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpochKey> {
    let topic_id = decode_fixed_32(row.get(0)?, 0, "topic_id")?;
    let epoch = decode_non_negative_epoch(row.get(1)?, 1)?;
    let key_data = decode_fixed_32(row.get(2)?, 2, "key_data")?;

    Ok(EpochKey {
        topic_id,
        epoch,
        key_data,
        received_at: row.get(3)?,
        expires_at: row.get(4)?,
    })
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
    let expires_at = now
        .checked_add(EPOCH_KEY_LIFETIME_SECS)
        .ok_or_else(|| rusqlite::Error::InvalidParameterName("expires_at overflow".to_string()))?;
    let epoch_i64 = encode_epoch_param(epoch)?;

    conn.execute(
        "INSERT OR REPLACE INTO epoch_keys (topic_id, epoch, key_data, received_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            topic_id.as_slice(),
            epoch_i64,
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
        params![topic_id.as_slice(), encode_epoch_param(epoch)?],
        parse_epoch_key_row,
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
            epoch.map(|e| decode_non_negative_epoch(e, 0)).transpose()
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
        parse_epoch_key_row,
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
        .query_map([topic_id.as_slice()], parse_epoch_key_row)?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(keys)
}

/// Delete expired epoch keys
pub fn cleanup_expired_epoch_keys(conn: &Connection) -> rusqlite::Result<usize> {
    let now = current_timestamp();
    let rows = conn.execute("DELETE FROM epoch_keys WHERE expires_at < ?1", [now])?;
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
    use crate::data::schema::{create_membership_tables, create_peer_table, create_topic_table};

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
        )
        .unwrap();
        // Insert topic
        conn.execute(
            "INSERT OR IGNORE INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            rusqlite::params![
                topic_id.as_slice(),
                harbor_id.as_slice(),
                admin_id.as_slice()
            ],
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

    #[test]
    fn test_store_epoch_key_rejects_epoch_out_of_i64_range() {
        let conn = setup_db();
        let topic_id = test_id(7);
        let key_data = test_id(121);
        create_test_topic(&conn, &topic_id);

        let err = store_epoch_key(&conn, &topic_id, (i64::MAX as u64) + 1, &key_data).unwrap_err();
        match err {
            rusqlite::Error::InvalidParameterName(name) => {
                assert!(name.contains("epoch out of i64 range"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_get_epoch_key_rejects_malformed_blob_lengths() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE epoch_keys (
                topic_id BLOB NOT NULL,
                epoch INTEGER NOT NULL,
                key_data BLOB NOT NULL,
                received_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                PRIMARY KEY (topic_id, epoch)
            )",
            [],
        )
        .unwrap();

        let topic_id = [1u8; 32];
        conn.execute(
            "INSERT INTO epoch_keys (topic_id, epoch, key_data, received_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![topic_id.as_slice(), 1i64, vec![2u8; 31], 0i64, 1i64],
        )
        .unwrap();

        let err = get_epoch_key(&conn, &topic_id, 1).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(idx, name, Type::Blob) => {
                assert_eq!(idx, 2);
                assert_eq!(name, "key_data");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_get_current_epoch_rejects_negative_epoch() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE epoch_keys (
                topic_id BLOB NOT NULL,
                epoch INTEGER NOT NULL,
                key_data BLOB NOT NULL,
                received_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                PRIMARY KEY (topic_id, epoch)
            )",
            [],
        )
        .unwrap();

        let topic_id = [9u8; 32];
        conn.execute(
            "INSERT INTO epoch_keys (topic_id, epoch, key_data, received_at, expires_at)
             VALUES (?1, -1, ?2, 0, 1)",
            params![topic_id.as_slice(), vec![3u8; 32]],
        )
        .unwrap();

        let err = get_current_epoch(&conn, &topic_id).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(idx, name, Type::Integer) => {
                assert_eq!(idx, 0);
                assert_eq!(name, "epoch");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_get_current_epoch_key_rejects_negative_epoch() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE epoch_keys (
                topic_id BLOB NOT NULL,
                epoch INTEGER NOT NULL,
                key_data BLOB NOT NULL,
                received_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                PRIMARY KEY (topic_id, epoch)
            )",
            [],
        )
        .unwrap();

        let topic_id = [8u8; 32];
        conn.execute(
            "INSERT INTO epoch_keys (topic_id, epoch, key_data, received_at, expires_at)
             VALUES (?1, -2, ?2, 0, 1)",
            params![topic_id.as_slice(), vec![4u8; 32]],
        )
        .unwrap();

        let err = get_current_epoch_key(&conn, &topic_id).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(idx, name, Type::Integer) => {
                assert_eq!(idx, 1);
                assert_eq!(name, "epoch");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
