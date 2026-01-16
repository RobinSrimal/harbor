//! Main Protocol implementation
//!
//! This is the core Protocol struct and initialization logic.
//! Implementation is split across:
//! - `protocol/` (this crate): Core struct, start/stop, public methods
//! - `handlers/`: Connection handling (incoming/outgoing)
//! - `tasks/`: Background automation

use std::sync::Arc;
use std::time::Duration;

use iroh::Endpoint;
use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::info;

use crate::data::{get_or_create_identity, start_db, LocalIdentity};
use crate::network::dht::{
    bootstrap_dial_info, bootstrap_node_ids, create_dht_node_with_buckets, ApiClient as DhtApiClient,
    Buckets, DhtConfig, DhtPool, DialInfo, PoolConfig, RpcClient as DhtRpcClient, DHT_ALPN,
};
use crate::network::harbor::protocol::HARBOR_ALPN;
use crate::network::send::protocol::SEND_ALPN;

use super::config::ProtocolConfig;
use super::types::{IncomingMessage, ProtocolError};

/// Maximum message size (512 KB)
pub const MAX_MESSAGE_SIZE: usize = 512 * 1024;

/// Retention period for Harbor packets (90 days)
///
/// How long packets are stored on Harbor Nodes before expiration.
#[allow(dead_code)]
pub const RETENTION_PERIOD: Duration = Duration::from_secs(90 * 24 * 60 * 60);

/// Retention period in seconds (for database queries)
#[allow(dead_code)]
pub const RETENTION_PERIOD_SECS: i64 = 90 * 24 * 60 * 60;

/// The Harbor Protocol
///
/// This is the main entry point for using the protocol.
pub struct Protocol {
    /// Configuration
    pub(crate) config: ProtocolConfig,
    /// Iroh endpoint
    pub(crate) endpoint: Endpoint,
    /// Local identity
    pub(crate) identity: LocalIdentity,
    /// Database connection (wrapped for thread safety)
    pub(crate) db: Arc<Mutex<Connection>>,
    /// DHT RPC client (must be kept alive to keep actor running)
    #[allow(dead_code)]
    dht_rpc_client: Option<DhtRpcClient>,
    /// DHT client for finding Harbor Nodes
    pub(crate) dht_client: Option<DhtApiClient>,
    /// Event sender
    pub(crate) event_tx: mpsc::Sender<IncomingMessage>,
    /// Event receiver (cloneable via Arc)
    event_rx: Arc<RwLock<Option<mpsc::Receiver<IncomingMessage>>>>,
    /// Running flag
    pub(crate) running: Arc<RwLock<bool>>,
    /// Background tasks
    pub(crate) tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
    /// Shutdown signal sender
    shutdown_tx: mpsc::Sender<()>,
}

impl Protocol {
    /// Start the protocol
    ///
    /// This initializes the database, creates an Iroh endpoint,
    /// and starts background tasks for DHT, Harbor sync, etc.
    pub async fn start(config: ProtocolConfig) -> Result<Self, ProtocolError> {
        // Initialize database
        let db_path = config
            .db_path
            .clone()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "harbor_protocol.db".to_string());

        // Generate or use provided key - convert to hex string for passphrase
        use zeroize::Zeroize;
        let mut db_key = config.db_key.unwrap_or_else(|| {
            // Generate a random key using rand
            use rand::{rngs::OsRng, Rng};
            let mut key = [0u8; 32];
            OsRng.fill(&mut key);
            key
        });
        let passphrase = hex::encode(&db_key);
        db_key.zeroize();

        let db = start_db(&db_path, &passphrase).map_err(|e| ProtocolError::Database(e.to_string()))?;

        // Get or create identity
        let identity =
            get_or_create_identity(&db).map_err(|e| ProtocolError::Database(e.to_string()))?;

        info!(
            endpoint_id = hex::encode(identity.public_key),
            created_at = identity.created_at,
            "Loaded identity from database"
        );

        // Create Iroh endpoint with our identity's secret key
        let secret_key = iroh::SecretKey::from_bytes(&identity.private_key);
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![
                SEND_ALPN.to_vec(),
                HARBOR_ALPN.to_vec(),
                DHT_ALPN.to_vec(),
            ])
            .bind()
            .await
            .map_err(|e| ProtocolError::StartFailed(format!("failed to create endpoint: {}", e)))?;

        info!(endpoint_id = %endpoint.node_id(), "Protocol started");

        // Initialize DHT
        let local_dht_id = crate::network::dht::Id::new(identity.public_key);
        let dht_pool = DhtPool::new(endpoint.clone(), PoolConfig::default());

        // Parse and register bootstrap nodes from config
        let mut bootstrap_ids: Vec<crate::network::dht::Id> = Vec::new();

        for bootstrap_str in &config.bootstrap_nodes {
            // Parse "endpoint_id" or "endpoint_id:relay_url" format
            let (endpoint_hex, relay_url) = if bootstrap_str.contains(':') {
                // Check if it's endpoint_id:relay_url format
                if let Some(colon_pos) = bootstrap_str.find(':') {
                    let first_part = &bootstrap_str[..colon_pos];
                    if first_part.len() == 64 && first_part.chars().all(|c| c.is_ascii_hexdigit()) {
                        (
                            first_part.to_string(),
                            Some(bootstrap_str[colon_pos + 1..].to_string()),
                        )
                    } else {
                        // Just an endpoint ID with colons (shouldn't happen, but handle it)
                        (bootstrap_str.clone(), None)
                    }
                } else {
                    (bootstrap_str.clone(), None)
                }
            } else {
                (bootstrap_str.clone(), None)
            };

            // Parse endpoint ID from hex
            if let Ok(bytes) = hex::decode(&endpoint_hex) {
                if bytes.len() == 32 {
                    let mut id_bytes = [0u8; 32];
                    id_bytes.copy_from_slice(&bytes);

                    // Create node ID
                    if let Ok(node_id) = iroh::NodeId::from_bytes(&id_bytes) {
                        // Register dial info with relay URL if available
                        let dial_info = if let Some(ref relay) = relay_url {
                            DialInfo::from_node_id_with_relay(node_id, relay.clone())
                        } else {
                            DialInfo::from_node_id(node_id)
                        };
                        dht_pool.register_dial_info(dial_info).await;

                        // Add to bootstrap IDs
                        bootstrap_ids.push(crate::network::dht::Id::new(id_bytes));

                        tracing::debug!(
                            endpoint = %endpoint_hex[..16],
                            relay = ?relay_url,
                            "Registered bootstrap node"
                        );
                    }
                }
            }
        }

        // If no custom bootstrap nodes, use defaults
        if bootstrap_ids.is_empty() {
            for (node_id, relay_url) in bootstrap_dial_info() {
                let dial_info = if let Some(relay) = relay_url {
                    DialInfo::from_node_id_with_relay(node_id, relay)
                } else {
                    DialInfo::from_node_id(node_id)
                };
                dht_pool.register_dial_info(dial_info).await;
            }
            bootstrap_ids = bootstrap_node_ids()
                .into_iter()
                .map(crate::network::dht::Id::new)
                .collect();
        }

        // Load persisted routing table and relay URLs from database
        let (persisted_buckets, persisted_relay_urls): (Option<Buckets>, Vec<([u8; 32], String)>) = 
            match crate::data::dht::load_routing_table_with_relays(&db) {
                Ok((bucket_vecs, relay_urls)) => {
                    // Convert Vec<Vec<[u8; 32]>> to Buckets
                    let mut buckets = Buckets::new();
                    let mut loaded_count = 0;
                    for (idx, entries) in bucket_vecs.into_iter().enumerate() {
                        if idx < crate::network::dht::BUCKET_COUNT {
                            for id_bytes in entries {
                                buckets
                                    .get_mut(idx)
                                    .add_node(crate::network::dht::Id::new(id_bytes));
                                loaded_count += 1;
                            }
                        }
                    }
                    if loaded_count > 0 {
                        info!(
                            entries = loaded_count,
                            relay_urls = relay_urls.len(),
                            "DHT: loaded routing table from database"
                        );
                        (Some(buckets), relay_urls)
                    } else {
                        (None, Vec::new())
                    }
                }
                Err(e) => {
                    info!(error = %e, "DHT: no persisted routing table (starting fresh)");
                    (None, Vec::new())
                }
            };

        let bootstrap_count = bootstrap_ids.len();

        let (dht_rpc_client, dht_client) = create_dht_node_with_buckets(
            local_dht_id,
            dht_pool,
            DhtConfig::persistent(),
            bootstrap_ids,
            persisted_buckets,
        );

        // Restore relay URLs from database
        if !persisted_relay_urls.is_empty() {
            if let Err(e) = dht_client.set_node_relay_urls(persisted_relay_urls).await {
                tracing::warn!(error = %e, "Failed to restore relay URLs from database");
            }
        }

        info!(
            bootstrap_count = bootstrap_count,
            "DHT initialized with relay info"
        );

        // Create event channel
        let (event_tx, event_rx) = mpsc::channel(1000);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        // Store both clients - rpc_client must be kept alive to keep actor running!
        let dht_rpc_client = Some(dht_rpc_client);
        let dht_client = Some(dht_client);

        let protocol = Self {
            config,
            endpoint,
            identity,
            db: Arc::new(Mutex::new(db)),
            dht_rpc_client,
            dht_client,
            event_tx,
            event_rx: Arc::new(RwLock::new(Some(event_rx))),
            running: Arc::new(RwLock::new(true)),
            tasks: Arc::new(RwLock::new(Vec::new())),
            shutdown_tx,
        };

        // Start background tasks
        protocol.start_background_tasks(shutdown_rx).await;

        Ok(protocol)
    }

    /// Stop the protocol
    pub async fn stop(&self) {
        info!("Stopping protocol...");

        // Signal shutdown
        {
            let mut running = self.running.write().await;
            *running = false;
        }

        // Send shutdown signal
        let _ = self.shutdown_tx.send(()).await;

        // Save DHT routing table before shutdown
        if let Some(ref dht_client) = self.dht_client {
            info!("Saving DHT routing table before shutdown...");
            if let Err(e) = Self::save_dht_routing_table(&self.db, dht_client).await {
                tracing::warn!(error = %e, "Failed to save DHT routing table on shutdown");
            }
        }

        // Cancel background tasks
        {
            let mut tasks = self.tasks.write().await;
            for task in tasks.drain(..) {
                task.abort();
            }
        }

        // Close endpoint
        self.endpoint.close().await;

        info!("Protocol stopped");
    }

    /// Get the event receiver
    ///
    /// This returns a receiver for incoming messages.
    /// Can only be called once - subsequent calls return None.
    pub async fn events(&self) -> Option<mpsc::Receiver<IncomingMessage>> {
        let mut rx = self.event_rx.write().await;
        rx.take()
    }

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the relay URL this node is connected to (if any)
    pub async fn relay_url(&self) -> Option<String> {
        let node_addr = self.endpoint.node_addr();
        node_addr.relay_url.map(|url| url.to_string())
    }

    /// Check if the protocol is running
    pub(crate) async fn check_running(&self) -> Result<(), ProtocolError> {
        let running = self.running.read().await;
        if !*running {
            return Err(ProtocolError::NotRunning);
        }
        Ok(())
    }

    /// Save the current DHT routing table to the database
    pub(crate) async fn save_dht_routing_table(
        db: &Arc<Mutex<Connection>>,
        dht_client: &DhtApiClient,
    ) -> Result<(), ProtocolError> {
        use crate::data::dht::{clear_all_entries, insert_dht_entry, DhtEntry};
        use crate::data::peer::{current_timestamp, upsert_peer, PeerInfo};
        use std::collections::HashMap;

        // Get current routing table from DHT actor
        let buckets = dht_client
            .get_routing_table()
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to get routing table: {}", e)))?;

        // Get relay URLs for nodes
        let relay_urls: HashMap<[u8; 32], String> = dht_client
            .get_node_relay_urls()
            .await
            .map_err(|e| ProtocolError::Network(format!("failed to get relay URLs: {}", e)))?
            .into_iter()
            .collect();

        let timestamp = current_timestamp();
        let mut entry_count = 0;

        // Save to database using transaction for atomicity
        let mut db_lock = db.lock().await;
        let tx = db_lock
            .transaction()
            .map_err(|e| ProtocolError::Database(format!("failed to start transaction: {}", e)))?;

        // Clear existing entries and insert new ones
        clear_all_entries(&tx)
            .map_err(|e| ProtocolError::Database(format!("failed to clear DHT entries: {}", e)))?;

        for (bucket_idx, entries) in buckets.iter().enumerate() {
            for endpoint_id in entries {
                // Ensure peer exists (required by foreign key constraint)
                let peer = PeerInfo {
                    endpoint_id: *endpoint_id,
                    endpoint_address: None,
                    address_timestamp: None,
                    last_latency_ms: None,
                    latency_timestamp: None,
                    last_seen: timestamp,
                };
                upsert_peer(&tx, &peer)
                    .map_err(|e| ProtocolError::Database(format!("failed to upsert peer: {}", e)))?;

                let entry = DhtEntry {
                    endpoint_id: *endpoint_id,
                    bucket_index: bucket_idx as u8,
                    added_at: timestamp,
                    relay_url: relay_urls.get(endpoint_id).cloned(),
                };
                insert_dht_entry(&tx, &entry).map_err(|e| {
                    ProtocolError::Database(format!("failed to insert DHT entry: {}", e))
                })?;
                entry_count += 1;
            }
        }

        tx.commit()
            .map_err(|e| ProtocolError::Database(format!("failed to commit transaction: {}", e)))?;

        info!(entries = entry_count, "DHT routing table saved to database");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;
    use crate::data::{
        add_topic_member, get_all_topics, get_topic_members, remove_topic_member, subscribe_topic,
        unsubscribe_topic,
    };
    use crate::protocol::TopicInvite;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_protocol_config() {
        let config = ProtocolConfig::for_testing();
        assert!(!config.enable_pow);
    }

    #[test]
    fn test_max_message_size_constant() {
        assert_eq!(MAX_MESSAGE_SIZE, 512 * 1024);
    }

    // ========== Topic Operations (DB-only tests) ==========

    #[test]
    fn test_create_topic_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let member_id = test_id(10);

        // Subscribe to topic
        subscribe_topic(&conn, &topic_id).unwrap();

        // Add member
        add_topic_member(&conn, &topic_id, &member_id).unwrap();

        // Verify
        let members = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0], member_id);
    }

    #[test]
    fn test_join_topic_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let existing_member = test_id(10);
        let new_member = test_id(11);

        // Simulate existing topic
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &existing_member).unwrap();

        // Join operation: add new member + existing members
        add_topic_member(&conn, &topic_id, &new_member).unwrap();

        // Verify
        let members = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&existing_member));
        assert!(members.contains(&new_member));
    }

    #[test]
    fn test_leave_topic_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let member1 = test_id(10);
        let member2 = test_id(11);

        // Setup topic with two members
        subscribe_topic(&conn, &topic_id).unwrap();
        add_topic_member(&conn, &topic_id, &member1).unwrap();
        add_topic_member(&conn, &topic_id, &member2).unwrap();

        // Member1 leaves (removes self from members)
        remove_topic_member(&conn, &topic_id, &member1).unwrap();

        // Verify member1 is removed but member2 remains
        let members = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(members.len(), 1);
        assert!(!members.contains(&member1));
        assert!(members.contains(&member2));

        // Now fully unsubscribe - this cascades and removes all members
        unsubscribe_topic(&conn, &topic_id).unwrap();

        // After unsubscribe, topic is gone (members cascade deleted)
        let members_after = get_topic_members(&conn, &topic_id).unwrap();
        assert!(members_after.is_empty());
    }

    #[test]
    fn test_list_topics_db_operations() {
        let conn = setup_test_db();

        // Create multiple topics
        for i in 0..5 {
            let topic_id = test_id(i);
            subscribe_topic(&conn, &topic_id).unwrap();
        }

        let topics = get_all_topics(&conn).unwrap();
        assert_eq!(topics.len(), 5);
    }

    #[test]
    fn test_get_invite_db_operations() {
        let conn = setup_test_db();
        let topic_id = test_id(1);
        let members = vec![test_id(10), test_id(11), test_id(12)];

        // Setup topic
        subscribe_topic(&conn, &topic_id).unwrap();
        for member in &members {
            add_topic_member(&conn, &topic_id, member).unwrap();
        }

        // Get members (simulates get_invite)
        let retrieved = get_topic_members(&conn, &topic_id).unwrap();
        assert_eq!(retrieved.len(), 3);
    }

    // ========== Error Condition Tests ==========

    #[test]
    fn test_topic_not_found_condition() {
        let conn = setup_test_db();
        let nonexistent = test_id(99);

        // Getting members for non-existent topic should return empty
        let members = get_topic_members(&conn, &nonexistent).unwrap();
        assert!(members.is_empty());
    }

    #[test]
    fn test_message_size_validation() {
        // Test that we have the right constant for validation
        let small_msg = vec![0u8; 1000];
        let max_msg = vec![0u8; MAX_MESSAGE_SIZE];
        let too_large = vec![0u8; MAX_MESSAGE_SIZE + 1];

        assert!(small_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(max_msg.len() <= MAX_MESSAGE_SIZE);
        assert!(too_large.len() > MAX_MESSAGE_SIZE);
    }

    // ========== TopicInvite Integration ==========

    #[test]
    fn test_topic_invite_from_db() {
        let conn = setup_test_db();
        let topic_id = test_id(42);
        let members = vec![test_id(1), test_id(2)];

        // Setup
        subscribe_topic(&conn, &topic_id).unwrap();
        for m in &members {
            add_topic_member(&conn, &topic_id, m).unwrap();
        }

        // Create invite from DB state
        let db_members = get_topic_members(&conn, &topic_id).unwrap();
        let invite = TopicInvite::new(topic_id, db_members);

        // Serialize and restore
        let hex = invite.to_hex().unwrap();
        let restored = TopicInvite::from_hex(&hex).unwrap();

        assert_eq!(restored.topic_id, topic_id);
        assert_eq!(restored.members.len(), 2);
    }
}
