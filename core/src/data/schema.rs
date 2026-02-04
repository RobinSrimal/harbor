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
    create_control_tables(conn)?;
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

/// Topic table: stores subscribed topics with their HarborID, admin, and members
///
/// All BLOB fields are 32 bytes (256-bit identifiers).
/// The admin_id references the peers table - the topic creator is the initial admin.
/// Note: Our own endpoint ID is inserted into peers table on init, so we can be admin.
pub fn create_topic_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS topics (
            topic_id BLOB PRIMARY KEY NOT NULL CHECK (length(topic_id) = 32),
            harbor_id BLOB NOT NULL CHECK (length(harbor_id) = 32),
            admin_id BLOB NOT NULL CHECK (length(admin_id) = 32),
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            FOREIGN KEY (admin_id) REFERENCES peers(endpoint_id)
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
            FOREIGN KEY (topic_id) REFERENCES topics(topic_id) ON DELETE CASCADE,
            FOREIGN KEY (endpoint_id) REFERENCES peers(endpoint_id)
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

/// Membership tables: epoch keys and pending decryption
///
/// All topics have an admin (the creator) who can:
/// - Invite/remove members
/// - Rotate epoch keys
/// - Transfer admin role
///
/// Current tables used:
/// - `topics` table (subscriptions, admin_id)
/// - `topic_members` table (member list from invite + Join/Leave messages)
/// - `epoch_keys` table (epoch keys per topic for decryption)
/// - `pending_decryption` table (packets awaiting epoch keys)
pub fn create_membership_tables(conn: &Connection) -> rusqlite::Result<()> {
    // Epoch keys table: stores encryption keys per topic per epoch
    // Keys are retained for 3 months, then pruned
    conn.execute(
        "CREATE TABLE IF NOT EXISTS epoch_keys (
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            epoch INTEGER NOT NULL,
            key_data BLOB NOT NULL CHECK (length(key_data) = 32),
            received_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            expires_at INTEGER NOT NULL,
            PRIMARY KEY (topic_id, epoch),
            FOREIGN KEY (topic_id) REFERENCES topics(topic_id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Pending decryption table: packets pulled from Harbor awaiting epoch keys
    // When the epoch key arrives, these packets are decrypted and processed
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pending_decryption (
            packet_id BLOB PRIMARY KEY NOT NULL CHECK (length(packet_id) = 32),
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            harbor_id BLOB NOT NULL CHECK (length(harbor_id) = 32),
            sender_id BLOB NOT NULL CHECK (length(sender_id) = 32),
            epoch INTEGER NOT NULL,
            packet_data BLOB NOT NULL,
            received_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            FOREIGN KEY (topic_id) REFERENCES topics(topic_id) ON DELETE CASCADE
        )",
        [],
    )?;

    // Index for finding pending packets by topic and epoch (for processing when key arrives)
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pending_topic_epoch
         ON pending_decryption(topic_id, epoch)",
        [],
    )?;

    // Index for finding expired epoch keys during cleanup
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_epoch_keys_expires
         ON epoch_keys(expires_at)",
        [],
    )?;

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
            scope_id BLOB NOT NULL CHECK (length(scope_id) = 32),
            source_id BLOB NOT NULL CHECK (length(source_id) = 32),
            display_name TEXT NOT NULL,
            total_size INTEGER NOT NULL,
            total_chunks INTEGER NOT NULL,
            num_sections INTEGER NOT NULL,
            state INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
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
        "CREATE INDEX IF NOT EXISTS idx_blobs_scope ON blobs(scope_id)",
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

/// Control tables: peer connections, tokens, and pending invites
///
/// Used by the Control ALPN for lifecycle and relationship management:
/// - `connection_list`: tracks peer relationships (connected, pending, blocked)
/// - `connect_tokens`: one-time tokens for QR code / invite string flow
/// - `pending_topic_invites`: topic invites awaiting user decision
pub fn create_control_tables(conn: &Connection) -> rusqlite::Result<()> {
    // Connection list: tracks peer relationships
    // States: pending_outgoing, pending_incoming, connected, declined, blocked
    conn.execute(
        "CREATE TABLE IF NOT EXISTS connection_list (
            peer_id BLOB PRIMARY KEY NOT NULL CHECK (length(peer_id) = 32),
            state TEXT NOT NULL CHECK (state IN ('pending_outgoing', 'pending_incoming', 'connected', 'declined', 'blocked')),
            display_name TEXT,
            relay_url TEXT,
            request_id BLOB CHECK (length(request_id) = 32),
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;

    // Index for filtering by state
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_connection_list_state ON connection_list(state)",
        [],
    )?;

    // Connect tokens: one-time secrets for QR code / invite string flow
    // Tokens live until consumed, no expiry
    conn.execute(
        "CREATE TABLE IF NOT EXISTS connect_tokens (
            token BLOB PRIMARY KEY NOT NULL CHECK (length(token) = 32),
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;

    // Pending topic invites: invites awaiting user decision
    conn.execute(
        "CREATE TABLE IF NOT EXISTS pending_topic_invites (
            message_id BLOB PRIMARY KEY NOT NULL CHECK (length(message_id) = 32),
            topic_id BLOB NOT NULL CHECK (length(topic_id) = 32),
            sender_id BLOB NOT NULL CHECK (length(sender_id) = 32),
            topic_name TEXT,
            epoch INTEGER NOT NULL,
            epoch_key BLOB NOT NULL CHECK (length(epoch_key) = 32),
            admin_id BLOB NOT NULL CHECK (length(admin_id) = 32),
            members_blob BLOB NOT NULL,
            received_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        )",
        [],
    )?;

    // Index for finding invites by topic
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pending_invites_topic ON pending_topic_invites(topic_id)",
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
        assert!(tables.contains(&"epoch_keys".to_string()));
        assert!(tables.contains(&"pending_decryption".to_string()));
        assert!(tables.contains(&"connection_list".to_string()));
        assert!(tables.contains(&"connect_tokens".to_string()));
        assert!(tables.contains(&"pending_topic_invites".to_string()));
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
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();

        // Insert admin peer first (for foreign key)
        let admin_id = [0u8; 32];
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![admin_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        // Valid 32-byte IDs should work
        let topic_id = [1u8; 32];
        let harbor_id = [2u8; 32];
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![topic_id.as_slice(), harbor_id.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        // Wrong size harbor_id should fail
        let bad_harbor = [3u8; 16];
        let result = conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![[4u8; 32].as_slice(), bad_harbor.as_slice(), admin_id.as_slice()],
        );
        assert!(result.is_err(), "Should reject harbor_id with wrong size");
    }

    // ========== Membership Tables Tests (Tier 1 - placeholder) ==========

    #[test]
    fn test_create_membership_tables() {
        let conn = in_memory_db();
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();
        create_membership_tables(&conn).unwrap();

        // Verify epoch_keys table exists
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"epoch_keys".to_string()));
        assert!(tables.contains(&"pending_decryption".to_string()));
    }

    #[test]
    fn test_epoch_keys_schema() {
        let conn = in_memory_db();
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();
        create_membership_tables(&conn).unwrap();

        let topic_id = [1u8; 32];
        let harbor_id = [2u8; 32];
        let admin_id = [3u8; 32];
        let key_data = [4u8; 32];

        // Insert admin peer first (for foreign key)
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![admin_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        // Insert topic
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![topic_id.as_slice(), harbor_id.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        // Insert epoch key
        let expires_at = 1704067200i64 + 7776000; // 90 days
        conn.execute(
            "INSERT INTO epoch_keys (topic_id, epoch, key_data, expires_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![topic_id.as_slice(), 1, key_data.as_slice(), expires_at],
        )
        .unwrap();

        // Query back
        let (retrieved_epoch, retrieved_key): (i64, Vec<u8>) = conn
            .query_row(
                "SELECT epoch, key_data FROM epoch_keys WHERE topic_id = ?1",
                [topic_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(retrieved_epoch, 1);
        assert_eq!(retrieved_key, key_data.to_vec());
    }

    #[test]
    fn test_pending_decryption_schema() {
        let conn = in_memory_db();
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();
        create_membership_tables(&conn).unwrap();

        let topic_id = [1u8; 32];
        let harbor_id = [2u8; 32];
        let admin_id = [3u8; 32];
        let packet_id = [4u8; 32];
        let sender_id = [5u8; 32];
        let packet_data = b"encrypted payload";

        // Insert admin peer first (for foreign key)
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            rusqlite::params![admin_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        // Insert topic
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![topic_id.as_slice(), harbor_id.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        // Insert pending packet
        conn.execute(
            "INSERT INTO pending_decryption (packet_id, topic_id, harbor_id, sender_id, epoch, packet_data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                packet_id.as_slice(),
                topic_id.as_slice(),
                harbor_id.as_slice(),
                sender_id.as_slice(),
                2,
                packet_data.as_slice()
            ],
        )
        .unwrap();

        // Query by topic and epoch (the main use case)
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_decryption WHERE topic_id = ?1 AND epoch = ?2",
                rusqlite::params![topic_id.as_slice(), 2],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, 1);
    }

    // ========== Control Tables Tests ==========

    #[test]
    fn test_create_control_tables() {
        let conn = in_memory_db();
        create_control_tables(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"connection_list".to_string()));
        assert!(tables.contains(&"connect_tokens".to_string()));
        assert!(tables.contains(&"pending_topic_invites".to_string()));
    }

    #[test]
    fn test_connection_list_schema() {
        let conn = in_memory_db();
        create_control_tables(&conn).unwrap();

        let peer_id = [1u8; 32];
        let request_id = [2u8; 32];

        // Insert a connection
        conn.execute(
            "INSERT INTO connection_list (peer_id, state, display_name, relay_url, request_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                peer_id.as_slice(),
                "connected",
                "Alice",
                "https://relay.example.com",
                request_id.as_slice()
            ],
        )
        .unwrap();

        // Query back
        let (state, name): (String, String) = conn
            .query_row(
                "SELECT state, display_name FROM connection_list WHERE peer_id = ?1",
                [peer_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(state, "connected");
        assert_eq!(name, "Alice");

        // Invalid state should fail
        let result = conn.execute(
            "INSERT INTO connection_list (peer_id, state) VALUES (?1, ?2)",
            rusqlite::params![[3u8; 32].as_slice(), "invalid_state"],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_connect_tokens_schema() {
        let conn = in_memory_db();
        create_control_tables(&conn).unwrap();

        let token = [42u8; 32];

        // Insert a token
        conn.execute(
            "INSERT INTO connect_tokens (token) VALUES (?1)",
            [token.as_slice()],
        )
        .unwrap();

        // Query back
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM connect_tokens WHERE token = ?1",
                [token.as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert!(exists);

        // Delete (consume) the token
        conn.execute(
            "DELETE FROM connect_tokens WHERE token = ?1",
            [token.as_slice()],
        )
        .unwrap();

        // Should be gone
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM connect_tokens WHERE token = ?1",
                [token.as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        assert!(!exists);
    }

    #[test]
    fn test_pending_topic_invites_schema() {
        let conn = in_memory_db();
        create_control_tables(&conn).unwrap();

        let message_id = [1u8; 32];
        let topic_id = [2u8; 32];
        let sender_id = [3u8; 32];
        let epoch_key = [4u8; 32];
        let admin_id = [5u8; 32];
        let members_blob: Vec<u8> = vec![]; // Empty member list

        // Insert a pending invite
        conn.execute(
            "INSERT INTO pending_topic_invites (message_id, topic_id, sender_id, topic_name, epoch, epoch_key, admin_id, members_blob)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                message_id.as_slice(),
                topic_id.as_slice(),
                sender_id.as_slice(),
                "Test Topic",
                1,
                epoch_key.as_slice(),
                admin_id.as_slice(),
                members_blob,
            ],
        )
        .unwrap();

        // Query back
        let (name, epoch): (String, i64) = conn
            .query_row(
                "SELECT topic_name, epoch FROM pending_topic_invites WHERE message_id = ?1",
                [message_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(name, "Test Topic");
        assert_eq!(epoch, 1);
    }
}

