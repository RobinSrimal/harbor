//! Harbor nodes cache
//!
//! Caches DHT lookup results for harbor node discovery.
//! Refreshed periodically by the background discovery task and fallback lookups.

use rusqlite::{Connection, params, types::Type};

fn decode_fixed_32(bytes: Vec<u8>, index: usize, name: &'static str) -> rusqlite::Result<[u8; 32]> {
    if bytes.len() != 32 {
        return Err(rusqlite::Error::InvalidColumnType(
            index,
            name.to_string(),
            Type::Blob,
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Get cached harbor nodes for a given harbor_id
pub fn get_cached_harbor_nodes(
    conn: &Connection,
    harbor_id: &[u8; 32],
) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT p.endpoint_id
         FROM harbor_nodes_cache h
         JOIN peers p ON h.peer_id = p.id
         WHERE h.harbor_id = ?1
         ORDER BY h.updated_at DESC",
    )?;

    let nodes = stmt
        .query_map([harbor_id.as_slice()], |row| {
            let node_id: Vec<u8> = row.get(0)?;
            decode_fixed_32(node_id, 0, "endpoint_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(nodes)
}

/// Replace all cached nodes for a harbor_id
///
/// This deletes existing entries and inserts the new nodes.
/// Called by discovery and by pull/sync fallback after a fresh DHT lookup.
pub fn replace_harbor_nodes(
    conn: &Connection,
    harbor_id: &[u8; 32],
    nodes: &[[u8; 32]],
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;

    // Delete existing entries for this harbor_id
    tx.execute(
        "DELETE FROM harbor_nodes_cache WHERE harbor_id = ?1",
        [harbor_id.as_slice()],
    )?;

    {
        // Insert new nodes
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO harbor_nodes_cache (harbor_id, peer_id, updated_at)
             VALUES (?1, (SELECT id FROM peers WHERE endpoint_id = ?2), strftime('%s', 'now'))",
        )?;

        for node_id in nodes {
            // Ensure the node exists in peers table first (for foreign key)
            tx.execute(
                "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, strftime('%s', 'now'))",
                [node_id.as_slice()],
            )?;

            stmt.execute(params![harbor_id.as_slice(), node_id.as_slice()])?;
        }
    }

    tx.commit()
}

/// Get all active harbor_ids from topics table
///
/// Returns the harbor_id for each topic we're subscribed to.
/// Used by the background discovery task to know which harbor_ids to refresh.
pub fn get_all_active_harbor_ids(conn: &Connection) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare("SELECT DISTINCT harbor_id FROM topics")?;

    let harbor_ids = stmt
        .query_map([], |row| {
            let harbor_id: Vec<u8> = row.get(0)?;
            decode_fixed_32(harbor_id, 0, "harbor_id")
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(harbor_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::{create_harbor_table, create_peer_table, create_topic_table};

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_peer_table(&conn).unwrap();
        create_topic_table(&conn).unwrap();
        create_harbor_table(&conn).unwrap();
        conn
    }

    #[test]
    fn test_get_cached_harbor_nodes_empty() {
        let conn = setup_db();
        let harbor_id = [1u8; 32];

        let nodes = get_cached_harbor_nodes(&conn, &harbor_id).unwrap();
        assert!(nodes.is_empty());
    }

    #[test]
    fn test_replace_and_get_harbor_nodes() {
        let conn = setup_db();
        let harbor_id = [1u8; 32];
        let node1 = [2u8; 32];
        let node2 = [3u8; 32];

        // Insert nodes
        replace_harbor_nodes(&conn, &harbor_id, &[node1, node2]).unwrap();

        // Get them back
        let nodes = get_cached_harbor_nodes(&conn, &harbor_id).unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.contains(&node1));
        assert!(nodes.contains(&node2));
    }

    #[test]
    fn test_replace_overwrites_existing() {
        let conn = setup_db();
        let harbor_id = [1u8; 32];
        let node1 = [2u8; 32];
        let node2 = [3u8; 32];
        let node3 = [4u8; 32];

        // Insert initial nodes
        replace_harbor_nodes(&conn, &harbor_id, &[node1, node2]).unwrap();

        // Replace with different nodes
        replace_harbor_nodes(&conn, &harbor_id, &[node3]).unwrap();

        // Should only have the new node
        let nodes = get_cached_harbor_nodes(&conn, &harbor_id).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0], node3);
    }

    #[test]
    fn test_get_all_active_harbor_ids() {
        let conn = setup_db();

        // Insert a peer for admin_id foreign key
        let admin_id = [1u8; 32];
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            [admin_id.as_slice()],
        )
        .unwrap();

        // Insert topics
        let topic1 = [10u8; 32];
        let harbor1 = [11u8; 32];
        let topic2 = [20u8; 32];
        let harbor2 = [21u8; 32];

        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            params![topic1.as_slice(), harbor1.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            params![topic2.as_slice(), harbor2.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        let harbor_ids = get_all_active_harbor_ids(&conn).unwrap();
        assert_eq!(harbor_ids.len(), 2);
        assert!(harbor_ids.contains(&harbor1));
        assert!(harbor_ids.contains(&harbor2));
    }

    #[test]
    fn test_get_all_active_harbor_ids_distinct() {
        let conn = setup_db();

        let admin_id = [1u8; 32];
        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            [admin_id.as_slice()],
        )
        .unwrap();

        let shared_harbor = [11u8; 32];
        let topic1 = [10u8; 32];
        let topic2 = [20u8; 32];
        let harbor2 = [21u8; 32];

        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            params![
                topic1.as_slice(),
                shared_harbor.as_slice(),
                admin_id.as_slice()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            params![
                topic2.as_slice(),
                shared_harbor.as_slice(),
                admin_id.as_slice()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id)
             VALUES (?1, ?2, (SELECT id FROM peers WHERE endpoint_id = ?3))",
            params![
                [30u8; 32].as_slice(),
                harbor2.as_slice(),
                admin_id.as_slice()
            ],
        )
        .unwrap();

        let harbor_ids = get_all_active_harbor_ids(&conn).unwrap();
        assert_eq!(harbor_ids.len(), 2);
        assert!(harbor_ids.contains(&shared_harbor));
        assert!(harbor_ids.contains(&harbor2));
    }

    #[test]
    fn test_replace_harbor_nodes_is_atomic_on_error() {
        let conn = setup_db();
        let harbor_id = [1u8; 32];
        let old_node = [2u8; 32];
        let good_node = [3u8; 32];
        let bad_node = [4u8; 32];

        replace_harbor_nodes(&conn, &harbor_id, &[old_node]).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            [bad_node.as_slice()],
        )
        .unwrap();
        let bad_peer_id: i64 = conn
            .query_row(
                "SELECT id FROM peers WHERE endpoint_id = ?1",
                [bad_node.as_slice()],
                |row| row.get(0),
            )
            .unwrap();

        let trigger_sql = format!(
            "CREATE TRIGGER abort_bad_harbor_node
             BEFORE INSERT ON harbor_nodes_cache
             WHEN NEW.peer_id = {}
             BEGIN
                 SELECT RAISE(ABORT, 'forced failure');
             END;",
            bad_peer_id
        );
        conn.execute(&trigger_sql, []).unwrap();

        let err = replace_harbor_nodes(&conn, &harbor_id, &[good_node, bad_node]).unwrap_err();
        let err_text = err.to_string();
        assert!(err_text.contains("forced failure"));

        let nodes = get_cached_harbor_nodes(&conn, &harbor_id).unwrap();
        assert_eq!(nodes, vec![old_node]);
    }

    #[test]
    fn test_get_cached_harbor_nodes_rejects_malformed_endpoint_id() {
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
            "CREATE TABLE harbor_nodes_cache (
                harbor_id BLOB NOT NULL,
                peer_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (harbor_id, peer_id)
            )",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO peers (endpoint_id, last_seen) VALUES (?1, 0)",
            [vec![9u8; 31]],
        )
        .unwrap();
        let peer_id: i64 = conn
            .query_row("SELECT id FROM peers LIMIT 1", [], |row| row.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO harbor_nodes_cache (harbor_id, peer_id, updated_at) VALUES (?1, ?2, 0)",
            params![vec![1u8; 32], peer_id],
        )
        .unwrap();

        let err = get_cached_harbor_nodes(&conn, &[1u8; 32]).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(_, name, Type::Blob) => {
                assert_eq!(name, "endpoint_id");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn test_get_all_active_harbor_ids_rejects_malformed_harbor_id() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE topics (
                topic_id BLOB PRIMARY KEY NOT NULL,
                harbor_id BLOB NOT NULL,
                admin_peer_id INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_peer_id) VALUES (?1, ?2, 1)",
            params![vec![2u8; 32], vec![3u8; 31]],
        )
        .unwrap();

        let err = get_all_active_harbor_ids(&conn).unwrap_err();
        match err {
            rusqlite::Error::InvalidColumnType(_, name, Type::Blob) => {
                assert_eq!(name, "harbor_id");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
