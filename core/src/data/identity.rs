//! Local node identity management
//!
//! Handles storage and retrieval of the local node's key pair.
//! The key pair is generated once on first run and persisted in the Protocol DB.
//!
//! # Security
//!
//! The private key is automatically zeroed from memory when `LocalIdentity`
//! is dropped, preventing key material from lingering in RAM.

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::security::create_key_pair::{KeyPair, generate_key_pair, key_pair_from_bytes};

/// Local node identity stored in the database
///
/// Implements `ZeroizeOnDrop` to securely clear the private key from memory.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct LocalIdentity {
    /// 32-byte private key (Ed25519)
    pub private_key: [u8; 32],
    /// 32-byte public key (Ed25519) - this is the EndpointID/NodeId
    #[zeroize(skip)]
    pub public_key: [u8; 32],
    /// When this identity was created
    #[zeroize(skip)]
    pub created_at: i64,
}

// Custom Debug to avoid exposing private key in logs
impl std::fmt::Debug for LocalIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalIdentity")
            .field("private_key", &"[REDACTED]")
            .field("public_key", &hex::encode(self.public_key))
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl LocalIdentity {
    /// Get the public key as a NodeId-compatible format
    pub fn node_id(&self) -> [u8; 32] {
        self.public_key
    }

    /// Convert to KeyPair for signing operations
    pub fn to_key_pair(&self) -> KeyPair {
        key_pair_from_bytes(&self.private_key)
    }
}

/// Get the local node identity, creating one if it doesn't exist
///
/// This is the main entry point for identity management.
/// On first run, generates a new key pair and stores it.
/// On subsequent runs, loads the existing key pair.
pub fn get_or_create_identity(conn: &Connection) -> rusqlite::Result<LocalIdentity> {
    // Try to load existing identity
    if let Some(identity) = get_identity(conn)? {
        return Ok(identity);
    }

    // No identity exists, create one
    let key_pair = generate_key_pair();
    match store_identity(conn, &key_pair) {
        Ok(()) => {}
        // If another process created identity first, recover by loading it.
        Err(rusqlite::Error::SqliteFailure(ref sqlite_err, _))
            if sqlite_err.code == ErrorCode::ConstraintViolation => {}
        Err(e) => return Err(e),
    }

    // Load and return the newly created identity
    get_identity(conn)?.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)
}

/// Get the existing local node identity, if any
pub fn get_identity(conn: &Connection) -> rusqlite::Result<Option<LocalIdentity>> {
    conn.query_row(
        "SELECT private_key, public_key, created_at FROM local_node WHERE id = 1",
        [],
        |row| {
            let private_key_vec: Vec<u8> = row.get(0)?;
            let public_key_vec: Vec<u8> = row.get(1)?;

            let mut private_key = [0u8; 32];
            let mut public_key = [0u8; 32];

            if private_key_vec.len() != 32 {
                return Err(rusqlite::Error::InvalidColumnType(
                    0,
                    "private_key".to_string(),
                    rusqlite::types::Type::Blob,
                ));
            }
            if public_key_vec.len() != 32 {
                return Err(rusqlite::Error::InvalidColumnType(
                    1,
                    "public_key".to_string(),
                    rusqlite::types::Type::Blob,
                ));
            }

            private_key.copy_from_slice(&private_key_vec);
            public_key.copy_from_slice(&public_key_vec);

            Ok(LocalIdentity {
                private_key,
                public_key,
                created_at: row.get(2)?,
            })
        },
    )
    .optional()
}

/// Store a new identity (internal use only)
fn store_identity(conn: &Connection, key_pair: &KeyPair) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO local_node (id, private_key, public_key) VALUES (1, ?1, ?2)",
        params![
            key_pair.private_key.as_slice(),
            key_pair.public_key.as_slice(),
        ],
    )?;
    Ok(())
}

/// Check if a local identity exists
pub fn has_identity(conn: &Connection) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM local_node WHERE id = 1", [], |row| {
        row.get(0)
    })?;
    Ok(count > 0)
}

/// Ensure our endpoint ID is in the peers table
///
/// This allows us to be the admin of topics we create and be a member of topics.
/// Uses INSERT OR IGNORE to handle the case where we're already in the table.
pub fn ensure_self_in_peers(conn: &Connection, endpoint_id: &[u8; 32]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, strftime('%s', 'now'))",
        params![endpoint_id.as_slice()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_local_node_table, create_peer_table};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_local_node_table(&conn).unwrap();
        conn
    }

    fn temp_db_path(name: &str) -> String {
        let temp_dir = std::env::temp_dir();
        format!("{}/harbor_test_{}.db", temp_dir.display(), name)
    }

    #[test]
    fn test_get_or_create_identity_creates_new() {
        let conn = setup_db();

        // Should not have identity initially
        assert!(!has_identity(&conn).unwrap());

        // Get or create should create one
        let identity = get_or_create_identity(&conn).unwrap();

        // Should now have identity
        assert!(has_identity(&conn).unwrap());

        // Keys should be 32 bytes
        assert_eq!(identity.private_key.len(), 32);
        assert_eq!(identity.public_key.len(), 32);

        // Private and public should be different
        assert_ne!(identity.private_key, identity.public_key);
    }

    #[test]
    fn test_get_or_create_identity_reuses_existing() {
        let conn = setup_db();

        // Create initial identity
        let identity1 = get_or_create_identity(&conn).unwrap();

        // Get again - should return the same identity
        let identity2 = get_or_create_identity(&conn).unwrap();

        assert_eq!(identity1.private_key, identity2.private_key);
        assert_eq!(identity1.public_key, identity2.public_key);
    }

    #[test]
    fn test_identity_persists_across_connections() {
        let db_path = temp_db_path("identity_persist");
        let _ = std::fs::remove_file(&db_path);

        let original_public_key: [u8; 32];

        // First connection - create identity
        {
            let conn = Connection::open(&db_path).unwrap();
            create_local_node_table(&conn).unwrap();
            let identity = get_or_create_identity(&conn).unwrap();
            original_public_key = identity.public_key;
        }

        // Second connection - should load same identity
        {
            let conn = Connection::open(&db_path).unwrap();
            let identity = get_or_create_identity(&conn).unwrap();
            assert_eq!(identity.public_key, original_public_key);
        }

        // Cleanup
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_identity_to_key_pair() {
        let conn = setup_db();
        let identity = get_or_create_identity(&conn).unwrap();

        let key_pair = identity.to_key_pair();

        assert_eq!(key_pair.private_key, identity.private_key);
        assert_eq!(key_pair.public_key, identity.public_key);
    }

    #[test]
    fn test_node_id() {
        let conn = setup_db();
        let identity = get_or_create_identity(&conn).unwrap();

        // node_id should return the public key
        assert_eq!(identity.node_id(), identity.public_key);
    }

    #[test]
    fn test_only_one_identity_allowed() {
        let conn = setup_db();

        // Create first identity
        get_or_create_identity(&conn).unwrap();

        // Try to insert another directly - should fail due to CHECK constraint
        let result = conn.execute(
            "INSERT INTO local_node (id, private_key, public_key) VALUES (2, ?1, ?2)",
            params![[0u8; 32].as_slice(), [1u8; 32].as_slice()],
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_get_identity_returns_none_when_empty() {
        let conn = setup_db();

        let identity = get_identity(&conn).unwrap();
        assert!(identity.is_none());
    }

    #[test]
    fn test_debug_does_not_expose_private_key() {
        let conn = setup_db();
        let identity = get_or_create_identity(&conn).unwrap();

        let debug_output = format!("{:?}", identity);

        // Should contain REDACTED, not actual key bytes
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug should redact private key"
        );

        // Should show public key in hex
        let public_hex = hex::encode(identity.public_key);
        assert!(
            debug_output.contains(&public_hex),
            "Debug should show public key"
        );
    }

    #[test]
    fn test_ensure_self_in_peers_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        create_peer_table(&conn).unwrap();

        let endpoint = [7u8; 32];
        ensure_self_in_peers(&conn, &endpoint).unwrap();
        ensure_self_in_peers(&conn, &endpoint).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM peers WHERE endpoint_id = ?1",
                [endpoint.as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, 1, "ensure_self_in_peers should not duplicate rows");
    }

    #[test]
    fn test_get_identity_rejects_invalid_private_key_size() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE local_node (
                id INTEGER PRIMARY KEY,
                private_key BLOB NOT NULL,
                public_key BLOB NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO local_node (id, private_key, public_key) VALUES (1, ?1, ?2)",
            params![[1u8; 16].as_slice(), [2u8; 32].as_slice()],
        )
        .unwrap();

        let err = get_identity(&conn).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(index, name, rusqlite::types::Type::Blob) => {
                assert_eq!(index, 0);
                assert_eq!(name, "private_key");
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn test_get_identity_rejects_invalid_public_key_size() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE local_node (
                id INTEGER PRIMARY KEY,
                private_key BLOB NOT NULL,
                public_key BLOB NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO local_node (id, private_key, public_key) VALUES (1, ?1, ?2)",
            params![[1u8; 32].as_slice(), [2u8; 16].as_slice()],
        )
        .unwrap();

        let err = get_identity(&conn).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(index, name, rusqlite::types::Type::Blob) => {
                assert_eq!(index, 1);
                assert_eq!(name, "public_key");
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }
}
