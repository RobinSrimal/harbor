//! Database schema definitions for Harbor Protocol DB
//!
//! All tables are created in SQLCipher encrypted database.

use rusqlite::Connection;

/// Creates all required database tables
pub fn create_all_tables(conn: &Connection) -> rusqlite::Result<()> {
    create_local_node_table(conn)?;
    create_peer_table(conn)?;
    create_dht_table(conn)?;
    create_topic_table(conn)?;
    create_outgoing_table(conn)?;
    create_harbor_table(conn)?;
    create_membership_tables(conn)?;
    create_share_tables(conn)?;
    Ok(())
}

/// Run database migrations for existing databases
///
/// This should be called after tables exist to add new columns, indexes, etc.
/// Each migration checks if it's already been applied before making changes.
pub fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    // Migration 1: Add packet_type column to outgoing_packets (v0.2)
    let has_packet_type: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('outgoing_packets') WHERE name = 'packet_type'",
        [],
        |row| row.get(0),
    )?;

    if !has_packet_type {
        conn.execute(
            "ALTER TABLE outgoing_packets ADD COLUMN packet_type INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }

    Ok(())
}

/// Local node table: stores this node's identity (key pair)
///
/// Only one row should exist - the local node's identity.
/// The private key is stored encrypted (by SQLCipher).
/// Both keys are 32 bytes (Ed25519).
pub fn create_local_node_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS local_node (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            private_key BLOB NOT NULL CHECK (length(private_key) = 32),
            public_key BLOB NOT NULL CHECK (length(public_key) = 32),
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;
    Ok(())
}

/// Peer table: stores EndpointID, EndpointAddress, and latency measurements
///
/// EndpointAddress is stored with a timestamp to enable direct dialing
/// when the address is less than 24 hours old.
/// EndpointID is 32 bytes (256-bit identifier).
pub fn create_peer_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS peers (
            endpoint_id BLOB PRIMARY KEY NOT NULL CHECK (length(endpoint_id) = 32),
            endpoint_address TEXT,
            address_timestamp INTEGER,
            last_latency_ms INTEGER,
            latency_timestamp INTEGER,
            last_seen INTEGER NOT NULL,
            relay_url TEXT,
            relay_url_last_success INTEGER,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;

    // Index for finding peers by last_seen for cleanup
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_peers_last_seen ON peers(last_seen)",
        [],
    )?;

    Ok(())
}

/// DHT table: stores routing table buckets
///
/// Each peer is assigned to a bucket based on XOR distance from local node.
/// Bucket 0 is furthest, bucket 255 is closest.
/// EndpointID is 32 bytes (256-bit identifier).
pub fn create_dht_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS dht_routing (
            endpoint_id BLOB PRIMARY KEY NOT NULL CHECK (length(endpoint_id) = 32),
            bucket_index INTEGER NOT NULL,
            added_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            FOREIGN KEY (endpoint_id) REFERENCES peers(endpoint_id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Index for efficient bucket queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_dht_bucket ON dht_routing(bucket_index)",
        [],
    )?;

    Ok(())
}

/// Topic table: stores subscribed topics with their HarborID and members
///
/// All BLOB fields are 32 bytes (256-bit identifiers).
pub fn create_topic_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS topics (
            topic_id BLOB PRIMARY KEY NOT NULL CHECK (length(topic_id) = 32),
            harbor_id BLOB NOT NULL CHECK (length(harbor_id) = 32),
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;

    // Topic members junction table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS topic_members (
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            endpoint_id BLOB NOT NULL CHECK (length(endpoint_id) = 32),
            joined_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            PRIMARY KEY (topic_id, endpoint_id),
            FOREIGN KEY (topic_id) REFERENCES topics(topic_id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Index for finding topics by harbor_id (used by Harbor Node lookups)
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_topics_harbor_id ON topics(harbor_id)",
        [],
    )?;

    Ok(())
}

/// Outgoing table: tracks sent packets and their delivery status
///
/// Used to track read receipts and determine when to replicate to Harbor Nodes.
/// All BLOB ID fields are 32 bytes (256-bit identifiers).
/// packet_type: 0=Content, 1=Join, 2=Leave (used for Harbor store requests)
pub fn create_outgoing_table(conn: &Connection) -> rusqlite::Result<()> {
    // Main outgoing packets table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS outgoing_packets (
            packet_id BLOB PRIMARY KEY NOT NULL CHECK (length(packet_id) = 32),
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            harbor_id BLOB NOT NULL CHECK (length(harbor_id) = 32),
            packet_data BLOB NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            replicated_to_harbor INTEGER NOT NULL DEFAULT 0,
            packet_type INTEGER NOT NULL DEFAULT 0
        )",
        [],
    )?;

    // Track which recipients have acknowledged (read receipts)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS outgoing_recipients (
            packet_id BLOB NOT NULL CHECK (length(packet_id) = 32),
            endpoint_id BLOB NOT NULL CHECK (length(endpoint_id) = 32),
            acknowledged INTEGER NOT NULL DEFAULT 0,
            acknowledged_at INTEGER,
            PRIMARY KEY (packet_id, endpoint_id),
            FOREIGN KEY (packet_id) REFERENCES outgoing_packets(packet_id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Index for finding packets needing harbor replication
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_outgoing_not_replicated 
         ON outgoing_packets(replicated_to_harbor) WHERE replicated_to_harbor = 0",
        [],
    )?;

    // Index for finding unacknowledged recipients
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_recipients_unacked 
         ON outgoing_recipients(acknowledged) WHERE acknowledged = 0",
        [],
    )?;

    Ok(())
}

/// Harbor cache table: stores packets for offline topic members
///
/// This node acts as a Harbor Node for HarborIDs where it has original packets.
/// Packets have a 3-month lifetime.
///
/// All BLOB ID fields are 32 bytes (256-bit identifiers).
pub fn create_harbor_table(conn: &Connection) -> rusqlite::Result<()> {
    // Main harbor cache table
    // packet_type: 0=Content, 1=Join, 2=Leave
    // synced: 0=not yet synced to partner harbor nodes, 1=synced
    //         Used for Harbor Protocol replication - ensures packets are
    //         distributed to multiple Harbor Nodes for redundancy
    conn.execute(
        "CREATE TABLE IF NOT EXISTS harbor_cache (
            packet_id BLOB PRIMARY KEY NOT NULL CHECK (length(packet_id) = 32),
            harbor_id BLOB NOT NULL CHECK (length(harbor_id) = 32),
            sender_id BLOB NOT NULL CHECK (length(sender_id) = 32),
            packet_data BLOB NOT NULL,
            packet_type INTEGER NOT NULL DEFAULT 0,
            synced INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            expires_at INTEGER NOT NULL
        )",
        [],
    )?;

    // Track which recipients need the packet
    conn.execute(
        "CREATE TABLE IF NOT EXISTS harbor_recipients (
            packet_id BLOB NOT NULL CHECK (length(packet_id) = 32),
            recipient_id BLOB NOT NULL CHECK (length(recipient_id) = 32),
            delivered INTEGER NOT NULL DEFAULT 0,
            delivered_at INTEGER,
            PRIMARY KEY (packet_id, recipient_id),
            FOREIGN KEY (packet_id) REFERENCES harbor_cache(packet_id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Track packets we've pulled from Harbor Nodes (client side)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pulled_packets (
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            packet_id BLOB NOT NULL CHECK (length(packet_id) = 32),
            pulled_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            PRIMARY KEY (topic_id, packet_id)
        )",
        [],
    )?;

    // Index for finding packets by harbor_id
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_harbor_harbor_id ON harbor_cache(harbor_id)",
        [],
    )?;

    // Index for finding expired packets
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_harbor_expires ON harbor_cache(expires_at)",
        [],
    )?;

    // Index for finding undelivered recipients
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_harbor_recipients_undelivered 
         ON harbor_recipients(delivered) WHERE delivered = 0",
        [],
    )?;

    Ok(())
}

/// Membership tables placeholder (Tier 2/3 - not used in Tier 1)
///
/// These tables are reserved for future tiers:
/// - Tier 2: Admin-managed topics with epoch-based keys
/// - Tier 3: Full MLS with TreeKEM
///
/// For Tier 1 (Open Topics), we only need:
/// - `topics` table (subscriptions)
/// - `topic_members` table (member list from invite + Join/Leave messages)
pub fn create_membership_tables(_conn: &Connection) -> rusqlite::Result<()> {
    // No additional tables needed for Tier 1
    // Future tiers will add: mls_group_state, epoch_keys, etc.
    Ok(())
}

/// Share tables: File sharing protocol for files ≥512 KB
///
/// Implements the pull-based distribution system with:
/// - `blobs`: File metadata (hash, size, sections, state)
/// - `blob_recipients`: Track which peers have received files
/// - `blob_sections`: Track section replication traces
pub fn create_share_tables(conn: &Connection) -> rusqlite::Result<()> {
    // Blob metadata table
    // State: 0=Partial, 1=Complete
    conn.execute(
        "CREATE TABLE IF NOT EXISTS blobs (
            hash BLOB PRIMARY KEY NOT NULL CHECK (length(hash) = 32),
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            source_id BLOB NOT NULL CHECK (length(source_id) = 32),
            display_name TEXT NOT NULL,
            total_size INTEGER NOT NULL,
            total_chunks INTEGER NOT NULL,
            num_sections INTEGER NOT NULL,
            state INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            FOREIGN KEY (topic_id) REFERENCES topics(topic_id)
        )",
        [],
    )?;

    // Track distribution status per recipient (for knowing when distribution is complete)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS blob_recipients (
            hash BLOB NOT NULL CHECK (length(hash) = 32),
            endpoint_id BLOB NOT NULL CHECK (length(endpoint_id) = 32),
            received INTEGER NOT NULL DEFAULT 0,
            received_at INTEGER,
            PRIMARY KEY (hash, endpoint_id),
            FOREIGN KEY (hash) REFERENCES blobs(hash) ON DELETE CASCADE
        )",
        [],
    )?;

    // Track section replication traces (section-level, not chunk-level)
    // Records who sent us each section for suggesting peers
    conn.execute(
        "CREATE TABLE IF NOT EXISTS blob_sections (
            hash BLOB NOT NULL CHECK (length(hash) = 32),
            section_id INTEGER NOT NULL,
            chunk_start INTEGER NOT NULL,
            chunk_end INTEGER NOT NULL,
            received_from BLOB CHECK (length(received_from) = 32),
            received_at INTEGER,
            PRIMARY KEY (hash, section_id),
            FOREIGN KEY (hash) REFERENCES blobs(hash) ON DELETE CASCADE
        )",
        [],
    )?;

    // Indexes for efficient queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_blobs_topic ON blobs(topic_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_blobs_state ON blobs(state)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_blob_recipients_pending 
         ON blob_recipients(received) WHERE received = 0",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_blob_sections_hash ON blob_sections(hash)",
        [],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn
    }

    #[test]
    fn test_create_all_tables() {
        let conn = in_memory_db();
        create_all_tables(&conn).unwrap();

        // Verify all tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"local_node".to_string()));
        assert!(tables.contains(&"peers".to_string()));
        assert!(tables.contains(&"dht_routing".to_string()));
        assert!(tables.contains(&"topics".to_string()));
        assert!(tables.contains(&"topic_members".to_string()));
        assert!(tables.contains(&"outgoing_packets".to_string()));
        assert!(tables.contains(&"outgoing_recipients".to_string()));
        assert!(tables.contains(&"harbor_cache".to_string()));
        assert!(tables.contains(&"harbor_recipients".to_string()));
        assert!(tables.contains(&"pulled_packets".to_string()));
    }

    #[test]
    fn test_peer_table_schema() {
        let conn = in_memory_db();
        create_peer_table(&conn).unwrap();

        // Insert a test peer
        let endpoint_id = [0u8; 32];
        conn.execute(
            "INSERT INTO peers (endpoint_id, endpoint_address, address_timestamp, last_seen) 
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                endpoint_id.as_slice(),
                "192.168.1.1:4433",
                1704067200i64, // 2024-01-01
                1704067200i64,
            ],
        )
        .unwrap();

        // Query it back
        let (retrieved_id, addr): (Vec<u8>, String) = conn
            .query_row(
                "SELECT endpoint_id, endpoint_address FROM peers WHERE endpoint_id = ?1",
                [endpoint_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(retrieved_id, endpoint_id.to_vec());
        assert_eq!(addr, "192.168.1.1:4433");
    }

    #[test]
    fn test_dht_table_schema() {
        let conn = in_memory_db();
        create_peer_table(&conn).unwrap();
        create_dht_table(&conn).unwrap();

        // Insert a peer first
        let endpoint_id = [1u8; 32];
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![endpoint_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        // Insert into DHT routing
        conn.execute(
            "INSERT INTO dht_routing (endpoint_id, bucket_index) VALUES (?1, ?2)",
            rusqlite::params![endpoint_id.as_slice(), 42],
        )
        .unwrap();

        // Query by bucket
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dht_routing WHERE bucket_index = ?1",
                [42],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn test_dht_cascade_delete() {
        let conn = in_memory_db();
        // Enable foreign keys
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_dht_table(&conn).unwrap();

        let endpoint_id = [2u8; 32];

        // Insert peer and DHT entry
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![endpoint_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO dht_routing (endpoint_id, bucket_index) VALUES (?1, ?2)",
            rusqlite::params![endpoint_id.as_slice(), 10],
        )
        .unwrap();

        // Delete peer - should cascade to DHT
        conn.execute(
            "DELETE FROM peers WHERE endpoint_id = ?1",
            [endpoint_id.as_slice()],
        )
        .unwrap();

        // DHT entry should be gone
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dht_routing", [], |row| row.get(0))
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn test_topics_harbor_id_index_exists() {
        let conn = in_memory_db();
        create_topic_table(&conn).unwrap();

        // Verify the index was created
        let indexes: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='topics'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(
            indexes.contains(&"idx_topics_harbor_id".to_string()),
            "harbor_id index should exist on topics table"
        );
    }

    #[test]
    fn test_blob_size_check_constraint() {
        let conn = in_memory_db();
        create_peer_table(&conn).unwrap();

        // Try to insert a peer with wrong endpoint_id size (16 bytes instead of 32)
        let short_id = [0u8; 16];
        let result = conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![short_id.as_slice(), 1704067200i64],
        );

        assert!(
            result.is_err(),
            "Should reject endpoint_id with wrong size"
        );
    }

    #[test]
    fn test_topic_table_check_constraints() {
        let conn = in_memory_db();
        create_topic_table(&conn).unwrap();

        // Valid 32-byte IDs should work
        let topic_id = [1u8; 32];
        let harbor_id = [2u8; 32];
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id) VALUES (?1, ?2)",
            rusqlite::params![topic_id.as_slice(), harbor_id.as_slice()],
        )
        .unwrap();

        // Wrong size harbor_id should fail
        let bad_harbor = [3u8; 16];
        let result = conn.execute(
            "INSERT INTO topics (topic_id, harbor_id) VALUES (?1, ?2)",
            rusqlite::params![[4u8; 32].as_slice(), bad_harbor.as_slice()],
        );
        assert!(result.is_err(), "Should reject harbor_id with wrong size");
    }

    // ========== Membership Tables Tests (Tier 1 - placeholder) ==========

    #[test]
    fn test_create_membership_tables_tier1() {
        let conn = in_memory_db();
        // Tier 1 doesn't create additional tables
        create_membership_tables(&conn).unwrap();
        // Function should succeed (no-op for Tier 1)
    }
}

