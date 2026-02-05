//! Harbor nodes cache
//!
//! Caches DHT lookup results for harbor node discovery.
//! Refreshed periodically by the background discovery task.

use rusqlite::{params, Connection};

/// Get cached harbor nodes for a given harbor_id
pub fn get_cached_harbor_nodes(
    conn: &Connection,
    harbor_id: &[u8; 32],
) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut stmt = conn.prepare(
        "SELECT node_id FROM harbor_nodes_cache WHERE harbor_id = ?1 ORDER BY updated_at DESC",
    )?;

    let nodes = stmt
        .query_map([harbor_id.as_slice()], |row| {
            let node_id: Vec<u8> = row.get(0)?;
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&node_id);
            Ok(arr)
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(nodes)
}

/// Replace all cached nodes for a harbor_id
///
/// This deletes existing entries and inserts the new nodes.
/// Called by the background discovery task after a fresh DHT lookup.
pub fn replace_harbor_nodes(
    conn: &Connection,
    harbor_id: &[u8; 32],
    nodes: &[[u8; 32]],
) -> rusqlite::Result<()> {
    // Delete existing entries for this harbor_id
    conn.execute(
        "DELETE FROM harbor_nodes_cache WHERE harbor_id = ?1",
        [harbor_id.as_slice()],
    )?;

    // Insert new nodes
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO harbor_nodes_cache (harbor_id, node_id, updated_at)
         VALUES (?1, ?2, strftime('%s', 'now'))",
    )?;

    for node_id in nodes {
        // Ensure the node exists in peers table first (for foreign key)
        conn.execute(
            "INSERT OR IGNORE INTO peers (endpoint_id, last_seen) VALUES (?1, strftime('%s', 'now'))",
            [node_id.as_slice()],
        )?;

        stmt.execute(params![harbor_id.as_slice(), node_id.as_slice()])?;
    }

    Ok(())
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
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&harbor_id);
            Ok(arr)
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
            "INSERT INTO topics (topic_id, harbor_id, admin_id) VALUES (?1, ?2, ?3)",
            params![topic1.as_slice(), harbor1.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO topics (topic_id, harbor_id, admin_id) VALUES (?1, ?2, ?3)",
            params![topic2.as_slice(), harbor2.as_slice(), admin_id.as_slice()],
        )
        .unwrap();

        let harbor_ids = get_all_active_harbor_ids(&conn).unwrap();
        assert_eq!(harbor_ids.len(), 2);
        assert!(harbor_ids.contains(&harbor1));
        assert!(harbor_ids.contains(&harbor2));
    }
}
