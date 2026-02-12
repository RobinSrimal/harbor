//! Connection gating for Harbor Protocol
//!
//! Provides fast in-memory lookup for connection authorization.
//! The gate checks if a peer is allowed to connect based on:
//! - DM connection status (from connection_list table)
//! - Topic membership (from topic_members table)
//!
//! A peer is allowed if they are DM-connected OR share any topic with us,
//! AND are not DM-blocked.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use crate::data::control::{ConnectionState, list_connections_by_state};
use crate::data::{get_all_topics, get_topic_members};

/// Fast connection authorization cache
///
/// Two separate tracking mechanisms with different semantics:
/// - DM connections: from connection_list (blocking here doesn't affect topics)
/// - Topic membership: from topic_members (separate access control)
pub struct ConnectionGate {
    /// DM-connected peers (from connection_list where state=Connected)
    dm_connected: RwLock<HashSet<[u8; 32]>>,
    /// Blocked peers for DM (from connection_list where state=Blocked)
    dm_blocked: RwLock<HashSet<[u8; 32]>>,
    /// Topic peers: peer_id -> set of shared topic_ids
    /// A peer appears here if they are a member of any topic we're also in
    topic_peers: RwLock<HashMap<[u8; 32], HashSet<[u8; 32]>>>,
    /// Our own endpoint ID (to exclude from topic_peers)
    our_id: [u8; 32],
    /// Database for loading initial state
    db: Arc<Mutex<DbConnection>>,
}

impl ConnectionGate {
    /// Create a new ConnectionGate and load initial state from database
    pub async fn new(db: Arc<Mutex<DbConnection>>, our_id: [u8; 32]) -> Self {
        let gate = Self {
            dm_connected: RwLock::new(HashSet::new()),
            dm_blocked: RwLock::new(HashSet::new()),
            topic_peers: RwLock::new(HashMap::new()),
            our_id,
            db,
        };
        gate.load_from_db().await;
        gate
    }

    /// Load connection states from database
    async fn load_from_db(&self) {
        let db = self.db.lock().await;

        // Load DM-connected peers
        match list_connections_by_state(&db, ConnectionState::Connected) {
            Ok(connected) => {
                let mut set = self.dm_connected.write().await;
                for conn in connected {
                    set.insert(conn.peer_id);
                }
                info!(count = set.len(), "gate: loaded DM-connected peers");
            }
            Err(err) => {
                warn!(error = %err, "gate: failed to load DM-connected peers");
            }
        }

        // Load DM-blocked peers
        match list_connections_by_state(&db, ConnectionState::Blocked) {
            Ok(blocked) => {
                let mut set = self.dm_blocked.write().await;
                for conn in blocked {
                    set.insert(conn.peer_id);
                }
                info!(count = set.len(), "gate: loaded DM-blocked peers");
            }
            Err(err) => {
                warn!(error = %err, "gate: failed to load DM-blocked peers");
            }
        }

        // Load topic membership
        // Get all topics we're subscribed to, then load their members
        match get_all_topics(&db) {
            Ok(topics) => {
                let mut peers_map = self.topic_peers.write().await;
                let mut total_peers = 0;
                let topic_count = topics.len();

                for topic in topics {
                    match get_topic_members(&db, &topic.topic_id) {
                        Ok(members) => {
                            for member in members {
                                // Don't add ourselves
                                if member == self.our_id {
                                    continue;
                                }
                                peers_map
                                    .entry(member)
                                    .or_insert_with(HashSet::new)
                                    .insert(topic.topic_id);
                                total_peers += 1;
                            }
                        }
                        Err(err) => {
                            warn!(
                                topic = %hex::encode(&topic.topic_id[..8]),
                                error = %err,
                                "gate: failed to load topic members"
                            );
                        }
                    }
                }
                info!(
                    topics = topic_count,
                    peer_entries = total_peers,
                    "gate: loaded topic membership"
                );
            }
            Err(err) => {
                warn!(error = %err, "gate: failed to load topics for membership gate");
            }
        }
    }

    /// Check if a peer is allowed to connect
    ///
    /// A peer is allowed if:
    /// - They are DM-connected OR share any topic with us
    /// - AND they are NOT DM-blocked
    pub async fn is_allowed(&self, peer_id: &[u8; 32]) -> bool {
        // Keep blocked guard alive until decision to avoid permissive transition windows.
        let blocked = self.dm_blocked.read().await;
        if blocked.contains(peer_id) {
            return false;
        }

        let connected = self.dm_connected.read().await;
        if connected.contains(peer_id) {
            return true;
        }
        drop(connected);

        // Check if shares any topic
        self.topic_peers.read().await.contains_key(peer_id)
    }

    /// Check if a peer is DM-connected (ignores topic membership)
    pub async fn is_dm_connected(&self, peer_id: &[u8; 32]) -> bool {
        self.dm_connected.read().await.contains(peer_id)
    }

    /// Check if a peer shares a specific topic with us
    pub async fn shares_topic(&self, peer_id: &[u8; 32], topic_id: &[u8; 32]) -> bool {
        if let Some(topics) = self.topic_peers.read().await.get(peer_id) {
            topics.contains(topic_id)
        } else {
            false
        }
    }

    /// Check if a peer is DM-blocked
    pub async fn is_blocked(&self, peer_id: &[u8; 32]) -> bool {
        self.dm_blocked.read().await.contains(peer_id)
    }

    // ============ DM State Updates ============

    /// Mark a peer as DM-connected
    ///
    /// Call this after accepting a connection request.
    /// Updates in-memory cache only - caller is responsible for DB update.
    pub async fn mark_dm_connected(&self, peer_id: &[u8; 32]) {
        let mut blocked = self.dm_blocked.write().await;
        let mut connected = self.dm_connected.write().await;
        blocked.remove(peer_id);
        connected.insert(*peer_id);
        debug!(peer = %hex::encode(&peer_id[..8]), "gate: marked DM-connected");
    }

    /// Mark a peer as DM-blocked
    ///
    /// Call this after blocking a peer.
    /// Updates in-memory cache only - caller is responsible for DB update.
    pub async fn mark_dm_blocked(&self, peer_id: &[u8; 32]) {
        let mut blocked = self.dm_blocked.write().await;
        let mut connected = self.dm_connected.write().await;
        blocked.insert(*peer_id);
        connected.remove(peer_id);
        debug!(peer = %hex::encode(&peer_id[..8]), "gate: marked DM-blocked");
    }

    /// Mark a peer as DM-disconnected (remove from connected, leave blocked alone)
    ///
    /// Call this after disconnecting from a peer (but not blocking).
    pub async fn mark_dm_disconnected(&self, peer_id: &[u8; 32]) {
        self.dm_connected.write().await.remove(peer_id);
        debug!(peer = %hex::encode(&peer_id[..8]), "gate: marked DM-disconnected");
    }

    /// Unblock a peer (remove from blocked)
    ///
    /// Call this after unblocking a peer.
    pub async fn unblock(&self, peer_id: &[u8; 32]) {
        self.dm_blocked.write().await.remove(peer_id);
        debug!(peer = %hex::encode(&peer_id[..8]), "gate: unblocked");
    }

    // ============ Topic Membership Updates ============

    /// Add a peer to a topic
    ///
    /// Call this when a peer joins a topic we're in.
    pub async fn add_topic_peer(&self, peer_id: &[u8; 32], topic_id: &[u8; 32]) {
        // Don't add ourselves
        if *peer_id == self.our_id {
            return;
        }

        self.topic_peers
            .write()
            .await
            .entry(*peer_id)
            .or_insert_with(HashSet::new)
            .insert(*topic_id);

        debug!(
            peer = %hex::encode(&peer_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            "gate: added topic peer"
        );
    }

    /// Remove a peer from a topic
    ///
    /// Call this when a peer leaves a topic we're in.
    pub async fn remove_topic_peer(&self, peer_id: &[u8; 32], topic_id: &[u8; 32]) {
        let mut peers = self.topic_peers.write().await;
        if let Some(topics) = peers.get_mut(peer_id) {
            topics.remove(topic_id);
            // If peer has no more topics with us, remove entirely
            if topics.is_empty() {
                peers.remove(peer_id);
            }
        }

        debug!(
            peer = %hex::encode(&peer_id[..8]),
            topic = %hex::encode(&topic_id[..8]),
            "gate: removed topic peer"
        );
    }

    /// Add multiple peers to a topic (batch operation)
    ///
    /// Call this when joining a topic with existing members.
    pub async fn add_topic_peers(&self, topic_id: &[u8; 32], peer_ids: &[[u8; 32]]) {
        let mut peers = self.topic_peers.write().await;
        let mut added = 0usize;
        for peer_id in peer_ids {
            // Don't add ourselves
            if *peer_id == self.our_id {
                continue;
            }
            peers
                .entry(*peer_id)
                .or_insert_with(HashSet::new)
                .insert(*topic_id);
            added += 1;
        }

        debug!(
            topic = %hex::encode(&topic_id[..8]),
            count = added,
            "gate: added topic peers (batch)"
        );
    }

    /// Remove all peers for a topic
    ///
    /// Call this when leaving a topic.
    pub async fn remove_topic(&self, topic_id: &[u8; 32]) {
        let mut peers = self.topic_peers.write().await;

        // Collect peers to potentially remove
        let mut empty_peers = Vec::new();

        for (peer_id, topics) in peers.iter_mut() {
            topics.remove(topic_id);
            if topics.is_empty() {
                empty_peers.push(*peer_id);
            }
        }

        // Remove peers with no remaining topics
        for peer_id in empty_peers {
            peers.remove(&peer_id);
        }

        debug!(
            topic = %hex::encode(&topic_id[..8]),
            "gate: removed all peers for topic"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;

    fn make_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    async fn setup_gate() -> ConnectionGate {
        let conn = DbConnection::open_in_memory().unwrap();
        create_all_tables(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));
        ConnectionGate::new(db, make_id(0)).await
    }

    #[tokio::test]
    async fn test_dm_connection_flow() {
        let gate = setup_gate().await;
        let peer = make_id(1);

        // Initially not allowed
        assert!(!gate.is_allowed(&peer).await);
        assert!(!gate.is_dm_connected(&peer).await);

        // Mark as DM-connected
        gate.mark_dm_connected(&peer).await;
        assert!(gate.is_allowed(&peer).await);
        assert!(gate.is_dm_connected(&peer).await);

        // Mark as blocked
        gate.mark_dm_blocked(&peer).await;
        assert!(!gate.is_allowed(&peer).await);
        assert!(gate.is_blocked(&peer).await);

        // Unblock
        gate.unblock(&peer).await;
        assert!(!gate.is_blocked(&peer).await);
        // But still not connected (blocking removed from connected)
        assert!(!gate.is_dm_connected(&peer).await);
    }

    #[tokio::test]
    async fn test_topic_membership_flow() {
        let gate = setup_gate().await;
        let peer = make_id(1);
        let topic1 = make_id(10);
        let topic2 = make_id(11);

        // Initially not allowed
        assert!(!gate.is_allowed(&peer).await);

        // Add to topic1
        gate.add_topic_peer(&peer, &topic1).await;
        assert!(gate.is_allowed(&peer).await);
        assert!(gate.shares_topic(&peer, &topic1).await);
        assert!(!gate.shares_topic(&peer, &topic2).await);

        // Add to topic2
        gate.add_topic_peer(&peer, &topic2).await;
        assert!(gate.shares_topic(&peer, &topic2).await);

        // Remove from topic1
        gate.remove_topic_peer(&peer, &topic1).await;
        assert!(!gate.shares_topic(&peer, &topic1).await);
        assert!(gate.is_allowed(&peer).await); // Still in topic2

        // Remove from topic2
        gate.remove_topic_peer(&peer, &topic2).await;
        assert!(!gate.is_allowed(&peer).await);
    }

    #[tokio::test]
    async fn test_dm_blocked_overrides_topic() {
        let gate = setup_gate().await;
        let peer = make_id(1);
        let topic = make_id(10);

        // Add to topic
        gate.add_topic_peer(&peer, &topic).await;
        assert!(gate.is_allowed(&peer).await);

        // Block for DM - should still block even with topic membership
        gate.mark_dm_blocked(&peer).await;
        assert!(!gate.is_allowed(&peer).await);
    }

    #[tokio::test]
    async fn test_dm_blocked_overrides_connected_and_topic() {
        let gate = setup_gate().await;
        let peer = make_id(1);
        let topic = make_id(10);

        gate.mark_dm_connected(&peer).await;
        gate.add_topic_peer(&peer, &topic).await;
        assert!(gate.is_allowed(&peer).await);

        gate.mark_dm_blocked(&peer).await;
        assert!(!gate.is_allowed(&peer).await);
        assert!(!gate.is_dm_connected(&peer).await);
        assert!(gate.shares_topic(&peer, &topic).await);
    }

    #[tokio::test]
    async fn test_batch_operations() {
        let gate = setup_gate().await;
        let topic = make_id(10);
        let peers = vec![make_id(1), make_id(2), make_id(3)];

        // Batch add
        gate.add_topic_peers(&topic, &peers).await;
        for peer in &peers {
            assert!(gate.is_allowed(peer).await);
        }

        // Remove topic
        gate.remove_topic(&topic).await;
        for peer in &peers {
            assert!(!gate.is_allowed(peer).await);
        }
    }

    #[tokio::test]
    async fn test_self_not_added() {
        let gate = setup_gate().await;
        let our_id = make_id(0); // Same as gate's our_id
        let topic = make_id(10);

        // Adding ourselves should be a no-op
        gate.add_topic_peer(&our_id, &topic).await;
        // We shouldn't be in the topic_peers map
        assert!(!gate.topic_peers.read().await.contains_key(&our_id));
    }

    #[tokio::test]
    async fn test_batch_add_skips_self() {
        let gate = setup_gate().await;
        let topic = make_id(10);
        let our_id = make_id(0);
        let peer = make_id(1);
        let peers = vec![our_id, peer];

        gate.add_topic_peers(&topic, &peers).await;

        assert!(!gate.topic_peers.read().await.contains_key(&our_id));
        assert!(gate.shares_topic(&peer, &topic).await);
        assert!(gate.is_allowed(&peer).await);
    }
}
