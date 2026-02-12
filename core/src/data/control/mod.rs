//! Data layer for Control ALPN
//!
//! Provides storage and retrieval for:
//! - Connection list (peer relationships)
//! - Connect tokens (QR code / invite string flow)
//! - Pending topic invites

use rusqlite::{Connection, OptionalExtension, params, types::Type};

use crate::data::dht::ensure_peer_exists;

// ============ Connection State Types ============

/// Connection state with a peer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// We sent a request, waiting for response
    PendingOutgoing,
    /// We received a request, waiting for our decision
    PendingIncoming,
    /// Mutual connection established
    Connected,
    /// Request was declined
    Declined,
    /// Peer is blocked
    Blocked,
}

impl ConnectionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnectionState::PendingOutgoing => "pending_outgoing",
            ConnectionState::PendingIncoming => "pending_incoming",
            ConnectionState::Connected => "connected",
            ConnectionState::Declined => "declined",
            ConnectionState::Blocked => "blocked",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending_outgoing" => Some(ConnectionState::PendingOutgoing),
            "pending_incoming" => Some(ConnectionState::PendingIncoming),
            "connected" => Some(ConnectionState::Connected),
            "declined" => Some(ConnectionState::Declined),
            "blocked" => Some(ConnectionState::Blocked),
            _ => None,
        }
    }
}

/// Information about a peer connection
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub peer_id: [u8; 32],
    pub state: ConnectionState,
    pub display_name: Option<String>,
    pub relay_url: Option<String>,
    pub request_id: Option<[u8; 32]>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn invalid_column_type(index: usize, name: &'static str, ty: Type) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(index, name.to_string(), ty)
}

fn decode_fixed_32(bytes: Vec<u8>, index: usize, name: &'static str) -> rusqlite::Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| invalid_column_type(index, name, Type::Blob))
}

fn decode_optional_fixed_32(
    bytes: Option<Vec<u8>>,
    index: usize,
    name: &'static str,
) -> rusqlite::Result<Option<[u8; 32]>> {
    match bytes {
        Some(v) => decode_fixed_32(v, index, name).map(Some),
        None => Ok(None),
    }
}

fn parse_connection_state(
    state: &str,
    index: usize,
    name: &'static str,
) -> rusqlite::Result<ConnectionState> {
    ConnectionState::from_str(state).ok_or_else(|| invalid_column_type(index, name, Type::Text))
}

fn parse_epoch(epoch: i64, index: usize, name: &'static str) -> rusqlite::Result<u64> {
    if epoch < 0 {
        return Err(invalid_column_type(index, name, Type::Integer));
    }
    Ok(epoch as u64)
}

fn encode_epoch(epoch: u64, index: usize, name: &'static str) -> rusqlite::Result<i64> {
    i64::try_from(epoch).map_err(|_| invalid_column_type(index, name, Type::Integer))
}

// ============ Connection List CRUD ============

/// Insert or update a connection in the list
pub fn upsert_connection(
    conn: &Connection,
    peer_id: &[u8; 32],
    state: ConnectionState,
    display_name: Option<&str>,
    relay_url: Option<&str>,
    request_id: Option<&[u8; 32]>,
) -> rusqlite::Result<()> {
    ensure_peer_exists(conn, peer_id)?;

    conn.execute(
        "INSERT INTO connection_list (peer_id, state, display_name, request_id, updated_at)
         VALUES ((SELECT id FROM peers WHERE endpoint_id = ?1), ?2, ?3, ?4, strftime('%s', 'now'))
         ON CONFLICT(peer_id) DO UPDATE SET
             state = excluded.state,
             display_name = COALESCE(excluded.display_name, connection_list.display_name),
             request_id = COALESCE(excluded.request_id, connection_list.request_id),
             updated_at = strftime('%s', 'now')",
        params![
            peer_id.as_slice(),
            state.as_str(),
            display_name,
            request_id.map(|r| r.as_slice()),
        ],
    )?;

    if let Some(relay) = relay_url {
        crate::data::update_peer_relay_url(conn, peer_id, relay, crate::data::current_timestamp())?;
    }

    Ok(())
}

/// Get connection info for a peer
pub fn get_connection(
    conn: &Connection,
    peer_id: &[u8; 32],
) -> rusqlite::Result<Option<ConnectionInfo>> {
    conn.query_row(
        "SELECT p.endpoint_id, c.state, c.display_name, p.relay_url, c.request_id, c.created_at, c.updated_at
         FROM connection_list c
         JOIN peers p ON c.peer_id = p.id
         WHERE p.endpoint_id = ?1",
        [peer_id.as_slice()],
        |row| {
            let peer_id_vec: Vec<u8> = row.get(0)?;
            let state_str: String = row.get(1)?;
            let request_id_opt: Option<Vec<u8>> = row.get(4)?;

            Ok(ConnectionInfo {
                peer_id: decode_fixed_32(peer_id_vec, 0, "endpoint_id")?,
                state: parse_connection_state(&state_str, 1, "state")?,
                display_name: row.get(2)?,
                relay_url: row.get(3)?,
                request_id: decode_optional_fixed_32(request_id_opt, 4, "request_id")?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        },
    )
    .optional()
}

/// Update connection state
pub fn update_connection_state(
    conn: &Connection,
    peer_id: &[u8; 32],
    state: ConnectionState,
) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "UPDATE connection_list SET state = ?1, updated_at = strftime('%s', 'now')
         WHERE peer_id = (SELECT id FROM peers WHERE endpoint_id = ?2)",
        params![state.as_str(), peer_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// List all connections with a given state
pub fn list_connections_by_state(
    conn: &Connection,
    state: ConnectionState,
) -> rusqlite::Result<Vec<ConnectionInfo>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id, c.state, c.display_name, p.relay_url, c.request_id, c.created_at, c.updated_at
         FROM connection_list c
         JOIN peers p ON c.peer_id = p.id
         WHERE c.state = ?1
         ORDER BY c.updated_at DESC",
    )?;

    let rows = stmt.query_map([state.as_str()], |row| {
        let peer_id_vec: Vec<u8> = row.get(0)?;
        let state_str: String = row.get(1)?;
        let request_id_opt: Option<Vec<u8>> = row.get(4)?;

        Ok(ConnectionInfo {
            peer_id: decode_fixed_32(peer_id_vec, 0, "endpoint_id")?,
            state: parse_connection_state(&state_str, 1, "state")?,
            display_name: row.get(2)?,
            relay_url: row.get(3)?,
            request_id: decode_optional_fixed_32(request_id_opt, 4, "request_id")?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;

    rows.collect()
}

/// List all connections
pub fn list_all_connections(conn: &Connection) -> rusqlite::Result<Vec<ConnectionInfo>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id, c.state, c.display_name, p.relay_url, c.request_id, c.created_at, c.updated_at
         FROM connection_list c
         JOIN peers p ON c.peer_id = p.id
         ORDER BY c.updated_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        let peer_id_vec: Vec<u8> = row.get(0)?;
        let state_str: String = row.get(1)?;
        let request_id_opt: Option<Vec<u8>> = row.get(4)?;

        Ok(ConnectionInfo {
            peer_id: decode_fixed_32(peer_id_vec, 0, "endpoint_id")?,
            state: parse_connection_state(&state_str, 1, "state")?,
            display_name: row.get(2)?,
            relay_url: row.get(3)?,
            request_id: decode_optional_fixed_32(request_id_opt, 4, "request_id")?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        })
    })?;

    rows.collect()
}

/// Delete a connection from the list
pub fn delete_connection(conn: &Connection, peer_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM connection_list WHERE peer_id = (SELECT id FROM peers WHERE endpoint_id = ?1)",
        [peer_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Check if a peer is connected (state = 'connected')
pub fn is_peer_connected(conn: &Connection, peer_id: &[u8; 32]) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM connection_list
         WHERE peer_id = (SELECT id FROM peers WHERE endpoint_id = ?1)
           AND state = 'connected'",
        [peer_id.as_slice()],
        |row| row.get(0),
    )
}

/// Check if a peer is blocked
pub fn is_peer_blocked(conn: &Connection, peer_id: &[u8; 32]) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM connection_list
         WHERE peer_id = (SELECT id FROM peers WHERE endpoint_id = ?1)
           AND state = 'blocked'",
        [peer_id.as_slice()],
        |row| row.get(0),
    )
}

// ============ Connect Tokens CRUD ============

/// Create a new connect token
pub fn create_connect_token(conn: &Connection, token: &[u8; 32]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO connect_tokens (token) VALUES (?1)",
        [token.as_slice()],
    )?;
    Ok(())
}

/// Check if a token exists (valid)
pub fn token_exists(conn: &Connection, token: &[u8; 32]) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM connect_tokens WHERE token = ?1",
        [token.as_slice()],
        |row| row.get(0),
    )
}

/// Consume (delete) a token - returns true if it existed
pub fn consume_token(conn: &Connection, token: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM connect_tokens WHERE token = ?1",
        [token.as_slice()],
    )?;
    Ok(rows > 0)
}

/// List all tokens (for debugging)
pub fn list_tokens(conn: &Connection) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare("SELECT token FROM connect_tokens")?;
    let rows = stmt.query_map([], |row| {
        let token_vec: Vec<u8> = row.get(0)?;
        decode_fixed_32(token_vec, 0, "token")
    })?;
    rows.collect()
}

// ============ Pending Topic Invites CRUD ============

/// Information about a pending topic invite
#[derive(Debug, Clone)]
pub struct PendingTopicInvite {
    pub message_id: [u8; 32],
    pub topic_id: [u8; 32],
    pub sender_id: [u8; 32],
    pub topic_name: Option<String>,
    pub epoch: u64,
    pub epoch_key: [u8; 32],
    pub admin_id: [u8; 32],
    pub members: Vec<[u8; 32]>,
    pub received_at: i64,
}

/// Store a pending topic invite
pub fn store_pending_invite(
    conn: &Connection,
    message_id: &[u8; 32],
    topic_id: &[u8; 32],
    sender_id: &[u8; 32],
    topic_name: Option<&str>,
    epoch: u64,
    epoch_key: &[u8; 32],
    admin_id: &[u8; 32],
    members: &[[u8; 32]],
) -> rusqlite::Result<()> {
    // Serialize members as concatenated 32-byte IDs
    let members_blob: Vec<u8> = members.iter().flat_map(|m| m.iter()).copied().collect();

    ensure_peer_exists(conn, sender_id)?;
    ensure_peer_exists(conn, admin_id)?;

    let epoch = encode_epoch(epoch, 4, "epoch")?;

    conn.execute(
        "INSERT OR REPLACE INTO pending_topic_invites
         (message_id, topic_id, sender_peer_id, topic_name, epoch, epoch_key, admin_peer_id, members_blob)
         VALUES (?1, ?2,
                 (SELECT id FROM peers WHERE endpoint_id = ?3),
                 ?4, ?5, ?6,
                 (SELECT id FROM peers WHERE endpoint_id = ?7),
                 ?8)",
        params![
            message_id.as_slice(),
            topic_id.as_slice(),
            sender_id.as_slice(),
            topic_name,
            epoch,
            epoch_key.as_slice(),
            admin_id.as_slice(),
            members_blob,
        ],
    )?;
    Ok(())
}

/// Parse members from concatenated 32-byte blob
fn parse_members_blob(blob: &[u8]) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut chunks = blob.chunks_exact(32);
    let members: Vec<[u8; 32]> = chunks
        .by_ref()
        .map(|chunk| {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(chunk);
            arr
        })
        .collect();

    if !chunks.remainder().is_empty() {
        return Err(invalid_column_type(7, "members_blob", Type::Blob));
    }

    Ok(members)
}

/// Get a pending invite by message_id
pub fn get_pending_invite(
    conn: &Connection,
    message_id: &[u8; 32],
) -> rusqlite::Result<Option<PendingTopicInvite>> {
    conn.query_row(
        "SELECT i.message_id, i.topic_id, s.endpoint_id, i.topic_name, i.epoch, i.epoch_key,
                a.endpoint_id, i.members_blob, i.received_at
         FROM pending_topic_invites i
         JOIN peers s ON i.sender_peer_id = s.id
         JOIN peers a ON i.admin_peer_id = a.id
         WHERE i.message_id = ?1",
        [message_id.as_slice()],
        |row| {
            let message_id_vec: Vec<u8> = row.get(0)?;
            let topic_id_vec: Vec<u8> = row.get(1)?;
            let sender_id_vec: Vec<u8> = row.get(2)?;
            let epoch_key_vec: Vec<u8> = row.get(5)?;
            let admin_id_vec: Vec<u8> = row.get(6)?;
            let members_blob: Vec<u8> = row.get(7)?;

            Ok(PendingTopicInvite {
                message_id: decode_fixed_32(message_id_vec, 0, "message_id")?,
                topic_id: decode_fixed_32(topic_id_vec, 1, "topic_id")?,
                sender_id: decode_fixed_32(sender_id_vec, 2, "endpoint_id")?,
                topic_name: row.get(3)?,
                epoch: parse_epoch(row.get::<_, i64>(4)?, 4, "epoch")?,
                epoch_key: decode_fixed_32(epoch_key_vec, 5, "epoch_key")?,
                admin_id: decode_fixed_32(admin_id_vec, 6, "endpoint_id")?,
                members: parse_members_blob(&members_blob)?,
                received_at: row.get(8)?,
            })
        },
    )
    .optional()
}

/// List all pending invites
pub fn list_pending_invites(conn: &Connection) -> rusqlite::Result<Vec<PendingTopicInvite>> {
    let mut stmt = conn.prepare(
        "SELECT i.message_id, i.topic_id, s.endpoint_id, i.topic_name, i.epoch, i.epoch_key,
                a.endpoint_id, i.members_blob, i.received_at
         FROM pending_topic_invites i
         JOIN peers s ON i.sender_peer_id = s.id
         JOIN peers a ON i.admin_peer_id = a.id
         ORDER BY i.received_at DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        let message_id_vec: Vec<u8> = row.get(0)?;
        let topic_id_vec: Vec<u8> = row.get(1)?;
        let sender_id_vec: Vec<u8> = row.get(2)?;
        let epoch_key_vec: Vec<u8> = row.get(5)?;
        let admin_id_vec: Vec<u8> = row.get(6)?;
        let members_blob: Vec<u8> = row.get(7)?;

        Ok(PendingTopicInvite {
            message_id: decode_fixed_32(message_id_vec, 0, "message_id")?,
            topic_id: decode_fixed_32(topic_id_vec, 1, "topic_id")?,
            sender_id: decode_fixed_32(sender_id_vec, 2, "endpoint_id")?,
            topic_name: row.get(3)?,
            epoch: parse_epoch(row.get::<_, i64>(4)?, 4, "epoch")?,
            epoch_key: decode_fixed_32(epoch_key_vec, 5, "epoch_key")?,
            admin_id: decode_fixed_32(admin_id_vec, 6, "endpoint_id")?,
            members: parse_members_blob(&members_blob)?,
            received_at: row.get(8)?,
        })
    })?;

    rows.collect()
}

/// Delete a pending invite (after accept/decline)
pub fn delete_pending_invite(conn: &Connection, message_id: &[u8; 32]) -> rusqlite::Result<bool> {
    let rows = conn.execute(
        "DELETE FROM pending_topic_invites WHERE message_id = ?1",
        [message_id.as_slice()],
    )?;
    Ok(rows > 0)
}

/// Check if we already have an invite for this topic
pub fn has_pending_invite_for_topic(
    conn: &Connection,
    topic_id: &[u8; 32],
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM pending_topic_invites WHERE topic_id = ?1",
        [topic_id.as_slice()],
        |row| row.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_control_tables;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        crate::data::schema::create_peer_table(&conn).unwrap();
        create_control_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_connection_crud() {
        let conn = setup_db();
        let peer_id = [1u8; 32];
        let request_id = [2u8; 32];

        // Insert
        upsert_connection(
            &conn,
            &peer_id,
            ConnectionState::PendingOutgoing,
            Some("Alice"),
            Some("https://relay.example.com"),
            Some(&request_id),
        )
        .unwrap();

        // Get
        let info = get_connection(&conn, &peer_id).unwrap().unwrap();
        assert_eq!(info.state, ConnectionState::PendingOutgoing);
        assert_eq!(info.display_name.as_deref(), Some("Alice"));

        // Update state
        update_connection_state(&conn, &peer_id, ConnectionState::Connected).unwrap();
        let info = get_connection(&conn, &peer_id).unwrap().unwrap();
        assert_eq!(info.state, ConnectionState::Connected);

        // Check connected
        assert!(is_peer_connected(&conn, &peer_id).unwrap());

        // List by state
        let connected = list_connections_by_state(&conn, ConnectionState::Connected).unwrap();
        assert_eq!(connected.len(), 1);

        // Delete
        delete_connection(&conn, &peer_id).unwrap();
        assert!(get_connection(&conn, &peer_id).unwrap().is_none());
    }

    #[test]
    fn test_token_crud() {
        let conn = setup_db();
        let token = [42u8; 32];

        // Create
        create_connect_token(&conn, &token).unwrap();
        assert!(token_exists(&conn, &token).unwrap());

        // List
        let tokens = list_tokens(&conn).unwrap();
        assert_eq!(tokens.len(), 1);

        // Consume
        assert!(consume_token(&conn, &token).unwrap());
        assert!(!token_exists(&conn, &token).unwrap());

        // Consume again should return false
        assert!(!consume_token(&conn, &token).unwrap());
    }

    #[test]
    fn test_pending_invite_crud() {
        let conn = setup_db();
        let message_id = [1u8; 32];
        let topic_id = [2u8; 32];
        let sender_id = [3u8; 32];
        let epoch_key = [4u8; 32];
        let admin_id = [5u8; 32];
        let members = vec![[3u8; 32], [6u8; 32]];

        // Store
        store_pending_invite(
            &conn,
            &message_id,
            &topic_id,
            &sender_id,
            Some("Test Topic"),
            1,
            &epoch_key,
            &admin_id,
            &members,
        )
        .unwrap();

        // Get
        let invite = get_pending_invite(&conn, &message_id).unwrap().unwrap();
        assert_eq!(invite.topic_name.as_deref(), Some("Test Topic"));
        assert_eq!(invite.epoch, 1);
        assert_eq!(invite.admin_id, admin_id);
        assert_eq!(invite.members.len(), 2);

        // Has invite for topic
        assert!(has_pending_invite_for_topic(&conn, &topic_id).unwrap());

        // List
        let invites = list_pending_invites(&conn).unwrap();
        assert_eq!(invites.len(), 1);

        // Delete
        delete_pending_invite(&conn, &message_id).unwrap();
        assert!(get_pending_invite(&conn, &message_id).unwrap().is_none());
    }

    #[test]
    fn test_get_connection_rejects_unknown_state_value() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE peers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id BLOB UNIQUE NOT NULL,
                relay_url TEXT,
                last_seen INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE connection_list (
                peer_id INTEGER PRIMARY KEY,
                state TEXT NOT NULL,
                display_name TEXT,
                request_id BLOB,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .unwrap();

        let peer_id = [9u8; 32];
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            params![peer_id.as_slice(), 1704067200i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO connection_list (peer_id, state)
             VALUES ((SELECT id FROM peers WHERE endpoint_id = ?1), ?2)",
            params![peer_id.as_slice(), "invalid_state"],
        )
        .unwrap();

        let err = get_connection(&conn, &peer_id).unwrap_err();
        assert!(matches!(
            err,
            rusqlite::Error::InvalidColumnType(_, name, Type::Text) if name == "state"
        ));
    }

    #[test]
    fn test_list_tokens_rejects_invalid_token_length() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE connect_tokens (
                token BLOB PRIMARY KEY NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO connect_tokens (token) VALUES (?1)",
            [[1u8; 16].as_slice()],
        )
        .unwrap();

        let err = list_tokens(&conn).unwrap_err();
        assert!(matches!(
            err,
            rusqlite::Error::InvalidColumnType(_, name, Type::Blob) if name == "token"
        ));
    }

    #[test]
    fn test_get_pending_invite_rejects_invalid_members_blob_length() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE peers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id BLOB UNIQUE NOT NULL,
                last_seen INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE pending_topic_invites (
                message_id BLOB PRIMARY KEY NOT NULL,
                topic_id BLOB NOT NULL,
                sender_peer_id INTEGER NOT NULL,
                topic_name TEXT,
                epoch INTEGER NOT NULL,
                epoch_key BLOB NOT NULL,
                admin_peer_id INTEGER NOT NULL,
                members_blob BLOB NOT NULL,
                received_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .unwrap();

        let message_id = [1u8; 32];
        let topic_id = [2u8; 32];
        let sender_id = [3u8; 32];
        let admin_id = [4u8; 32];
        let epoch_key = [5u8; 32];

        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            params![sender_id.as_slice(), 1704067200i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            params![admin_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO pending_topic_invites
             (message_id, topic_id, sender_peer_id, topic_name, epoch, epoch_key, admin_peer_id, members_blob)
             VALUES (?1, ?2,
                     (SELECT id FROM peers WHERE endpoint_id = ?3),
                     ?4, ?5, ?6,
                     (SELECT id FROM peers WHERE endpoint_id = ?7),
                     ?8)",
            params![
                message_id.as_slice(),
                topic_id.as_slice(),
                sender_id.as_slice(),
                "Bad Members",
                1i64,
                epoch_key.as_slice(),
                admin_id.as_slice(),
                vec![0u8; 33],
            ],
        )
        .unwrap();

        let err = get_pending_invite(&conn, &message_id).unwrap_err();
        assert!(matches!(
            err,
            rusqlite::Error::InvalidColumnType(_, name, Type::Blob) if name == "members_blob"
        ));
    }

    #[test]
    fn test_get_pending_invite_rejects_negative_epoch() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE peers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                endpoint_id BLOB UNIQUE NOT NULL,
                last_seen INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE pending_topic_invites (
                message_id BLOB PRIMARY KEY NOT NULL,
                topic_id BLOB NOT NULL,
                sender_peer_id INTEGER NOT NULL,
                topic_name TEXT,
                epoch INTEGER NOT NULL,
                epoch_key BLOB NOT NULL,
                admin_peer_id INTEGER NOT NULL,
                members_blob BLOB NOT NULL,
                received_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .unwrap();

        let message_id = [11u8; 32];
        let topic_id = [12u8; 32];
        let sender_id = [13u8; 32];
        let admin_id = [14u8; 32];
        let epoch_key = [15u8; 32];

        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            params![sender_id.as_slice(), 1704067200i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, ?2)",
            params![admin_id.as_slice(), 1704067200i64],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO pending_topic_invites
             (message_id, topic_id, sender_peer_id, topic_name, epoch, epoch_key, admin_peer_id, members_blob)
             VALUES (?1, ?2,
                     (SELECT id FROM peers WHERE endpoint_id = ?3),
                     ?4, ?5, ?6,
                     (SELECT id FROM peers WHERE endpoint_id = ?7),
                     ?8)",
            params![
                message_id.as_slice(),
                topic_id.as_slice(),
                sender_id.as_slice(),
                "Bad Epoch",
                -1i64,
                epoch_key.as_slice(),
                admin_id.as_slice(),
                vec![],
            ],
        )
        .unwrap();

        let err = get_pending_invite(&conn, &message_id).unwrap_err();
        assert!(matches!(
            err,
            rusqlite::Error::InvalidColumnType(_, name, Type::Integer) if name == "epoch"
        ));
    }

    #[test]
    fn test_store_pending_invite_rejects_epoch_out_of_i64_range() {
        let conn = setup_db();
        let message_id = [21u8; 32];
        let topic_id = [22u8; 32];
        let sender_id = [23u8; 32];
        let epoch_key = [24u8; 32];
        let admin_id = [25u8; 32];

        let err = store_pending_invite(
            &conn,
            &message_id,
            &topic_id,
            &sender_id,
            Some("Overflow Epoch"),
            (i64::MAX as u64) + 1,
            &epoch_key,
            &admin_id,
            &[],
        )
        .unwrap_err();

        assert!(matches!(
            err,
            rusqlite::Error::InvalidColumnType(_, name, Type::Integer) if name == "epoch"
        ));
        assert!(get_pending_invite(&conn, &message_id).unwrap().is_none());
    }
}
