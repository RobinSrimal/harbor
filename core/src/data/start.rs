//! Database initialization and startup
//!
//! Opens the SQLCipher encrypted database and ensures all required tables exist.
//!
//! # Passphrase Handling
//!
//! The passphrase should be obtained from one of:
//! - Interactive prompt (most secure, use `rpassword` crate)
//! - Environment variable `HARBOR_DB_KEY` (for daemons/scripts)
//! - OS keychain (future "remember me" feature)
//!
//! **Never** accept passphrase as a command-line argument (visible in `ps`).

use rusqlite::Connection;

use super::schema::create_all_tables;

/// Error type for database startup
#[derive(Debug)]
pub enum StartError {
    /// Empty passphrase provided
    EmptyPassphrase,
    /// SQLite/SQLCipher error
    Database(rusqlite::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::EmptyPassphrase => write!(f, "passphrase cannot be empty"),
            StartError::Database(e) => write!(f, "database error: {}", e),
        }
    }
}

impl std::error::Error for StartError {}

impl From<rusqlite::Error> for StartError {
    fn from(e: rusqlite::Error) -> Self {
        StartError::Database(e)
    }
}

/// Opens the encrypted database and ensures all required tables exist
///
/// # Arguments
/// * `db_path` - Path to the database file
/// * `passphrase` - Encryption passphrase for SQLCipher (must not be empty)
///
/// # Returns
/// A configured SQLite connection with all tables created
///
/// # Errors
/// - `StartError::EmptyPassphrase` if passphrase is empty
/// - `StartError::Database` for SQLite/SQLCipher errors
///
/// # Security
/// SQL PRAGMA values cannot be bound as query parameters, so the passphrase
/// is first hex-encoded before interpolation into `PRAGMA key`.
/// This constrains the interpolated content to safe hex characters and
/// avoids quote/escape injection.
pub fn start_db(db_path: &str, passphrase: &str) -> Result<Connection, StartError> {
    // Validate passphrase
    if passphrase.is_empty() {
        return Err(StartError::EmptyPassphrase);
    }

    let conn = Connection::open(db_path)?;

    // Set the encryption key using hex encoding to prevent SQL injection
    // SQLCipher accepts: PRAGMA key = "x'hexstring'"
    // This safely handles any passphrase characters
    let hex_key = hex::encode(passphrase.as_bytes());
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))?;

    // Enable WAL mode for better concurrency (multiple readers, non-blocking writes)
    // Note: PRAGMA returns the new mode, so we use query_row instead of execute
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;

    // Enable foreign keys
    conn.execute("PRAGMA foreign_keys = ON", [])?;

    // Schema creation is idempotent (`CREATE TABLE IF NOT EXISTS`), so always
    // run it to recover cleanly from partially initialized databases.
    create_all_tables(&conn)?;

    // No migration step yet: pre-testnet schema changes are handled by fresh table creation.
    Ok(conn)
}

/// Create an in-memory database for testing (unencrypted)
pub fn start_memory_db() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    // Note: WAL mode doesn't work with in-memory databases, skip it
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    create_all_tables(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_db_path(name: &str) -> String {
        let temp_dir = std::env::temp_dir();
        format!("{}/harbor_test_{}.db", temp_dir.display(), name)
    }

    fn cleanup(path: &str) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_empty_passphrase_rejected() {
        let db_path = temp_db_path("empty_pass");
        cleanup(&db_path);

        let result = start_db(&db_path, "");
        assert!(matches!(result, Err(StartError::EmptyPassphrase)));

        cleanup(&db_path);
    }

    #[test]
    fn test_passphrase_with_special_chars() {
        // Test that special characters don't cause SQL injection
        let db_path = temp_db_path("special_chars");
        cleanup(&db_path);

        // This passphrase would break naive string formatting
        let dangerous_passphrase = "test'; DROP TABLE peers; --";
        let result = start_db(&db_path, dangerous_passphrase);

        // Should succeed (hex encoding handles special chars)
        assert!(result.is_ok(), "passphrase with special chars should work");

        cleanup(&db_path);
    }

    #[test]
    fn test_passphrase_with_quotes() {
        let db_path = temp_db_path("quotes");
        cleanup(&db_path);

        let passphrase_with_quotes = r#"my"pass'phrase"#;
        let result = start_db(&db_path, passphrase_with_quotes);
        assert!(result.is_ok(), "passphrase with quotes should work");

        cleanup(&db_path);
    }

    #[test]
    fn test_creates_all_tables_on_new_db() {
        let db_path = temp_db_path("new_db_v3");
        cleanup(&db_path);

        let conn = start_db(&db_path, "test-passphrase").unwrap();

        // Verify all tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(
            tables.contains(&"local_node".to_string()),
            "local_node table missing"
        );
        assert!(tables.contains(&"peers".to_string()), "peers table missing");
        assert!(
            tables.contains(&"dht_routing".to_string()),
            "dht_routing table missing"
        );
        assert!(
            tables.contains(&"topics".to_string()),
            "topics table missing"
        );
        assert!(
            tables.contains(&"topic_members".to_string()),
            "topic_members table missing"
        );

        cleanup(&db_path);
    }

    #[test]
    fn test_reopening_db_preserves_data() {
        let db_path = temp_db_path("reopen_v3");
        cleanup(&db_path);

        // First open - create and insert
        {
            let conn = start_db(&db_path, "test-passphrase").unwrap();
            conn.execute(
                "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
                rusqlite::params![[1u8; 32].as_slice(), 1704067200i64],
            )
            .unwrap();
        }

        // Second open - data should still be there
        {
            let conn = start_db(&db_path, "test-passphrase").unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM peers", [], |row| row.get(0))
                .unwrap();

            assert_eq!(count, 1);
        }

        cleanup(&db_path);
    }

    #[test]
    fn test_doesnt_recreate_existing_tables() {
        let db_path = temp_db_path("no_recreate_v3");
        cleanup(&db_path);

        // First open - create tables and insert data
        {
            let conn = start_db(&db_path, "test-passphrase").unwrap();
            conn.execute(
                "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
                rusqlite::params![[2u8; 32].as_slice(), 1704067200i64],
            )
            .unwrap();
        }

        // Second open - should not recreate tables (data would be lost)
        {
            let conn = start_db(&db_path, "test-passphrase").unwrap();

            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM peers", [], |row| row.get(0))
                .unwrap();

            assert_eq!(count, 1, "data should be preserved, table not recreated");
        }

        cleanup(&db_path);
    }

    #[test]
    fn test_recovers_partial_schema_when_peers_exists() {
        let db_path = temp_db_path("partial_schema_v1");
        cleanup(&db_path);

        // Simulate a partially initialized database where only `peers` remains.
        {
            let conn = start_db(&db_path, "test-passphrase").unwrap();
            conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('peers', 'sqlite_sequence')")
                .unwrap();
            let tables_to_drop: Vec<String> = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            drop(stmt);

            for table in tables_to_drop {
                conn.execute(&format!("DROP TABLE IF EXISTS \"{}\"", table), [])
                    .unwrap();
            }
            conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        }

        let conn = start_db(&db_path, "test-passphrase").unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(
            tables.contains(&"local_node".to_string()),
            "local_node table missing"
        );
        assert!(
            tables.contains(&"topics".to_string()),
            "topics table missing"
        );
        assert!(
            tables.contains(&"outgoing_packets".to_string()),
            "outgoing_packets table missing"
        );
        assert!(
            tables.contains(&"harbor_cache".to_string()),
            "harbor_cache table missing"
        );

        cleanup(&db_path);
    }

    #[test]
    fn test_memory_db() {
        let conn = start_memory_db().unwrap();

        // Should include the core tables that span all data subdomains.
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(
            tables.contains(&"local_node".to_string()),
            "local_node table missing"
        );
        assert!(tables.contains(&"peers".to_string()), "peers table missing");
        assert!(
            tables.contains(&"dht_routing".to_string()),
            "dht_routing table missing"
        );
        assert!(
            tables.contains(&"outgoing_packets".to_string()),
            "outgoing_packets table missing"
        );
        assert!(
            tables.contains(&"harbor_cache".to_string()),
            "harbor_cache table missing"
        );
        assert!(tables.contains(&"blobs".to_string()), "blobs table missing");
        assert!(
            tables.contains(&"connection_list".to_string()),
            "connection_list table missing"
        );
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let conn = start_memory_db().unwrap();

        let fk_enabled: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();

        assert_eq!(fk_enabled, 1, "foreign keys should be enabled");
    }

    #[test]
    fn test_error_display() {
        let empty_err = StartError::EmptyPassphrase;
        assert_eq!(format!("{}", empty_err), "passphrase cannot be empty");
    }
}
