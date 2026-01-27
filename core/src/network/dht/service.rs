//! DHT Service - single entry point for all DHT operations
//!
//! Wraps the DHT actor, connection pool, and provides both
//! incoming and outgoing DHT protocol handling.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tracing::info;

use super::internal::api::{
    ApiProtocol, AddCandidates, FindClosest, GetNodeRelayUrls, GetRoutingTable,
    HandleFindNodeRequest, Lookup, NodesDead, NodesSeen, RandomLookup, SelfLookup,
    SetNodeRelayUrls,
};
use super::internal::distance::Id;
use super::internal::pool::{DhtPool, DhtPoolError as PoolError, DialInfo};
use super::protocol::{FindNode, FindNodeResponse, NodeInfo, RpcProtocol};
use crate::data::dht::{clear_all_entries, current_timestamp, insert_dht_entry, upsert_peer, DhtEntry, PeerInfo};
use crate::network::rpc::ExistingConnection;

/// DHT Service - the single entry point for all DHT operations
///
/// Owns the connection pool and actor channel. All DHT operations
/// (incoming handlers, outgoing RPCs, lookups) go through this service.
pub struct DhtService {
    /// Connection pool for DHT peers
    pool: DhtPool,
    /// irpc client for sending commands to the actor
    client: irpc::Client<ApiProtocol>,
    /// Database connection for persistence
    db: Arc<Mutex<DbConnection>>,
}

impl DhtService {
    /// Create a new DHT service
    pub fn new(pool: DhtPool, client: irpc::Client<ApiProtocol>, db: Arc<Mutex<DbConnection>>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            client,
            db,
        })
    }

    /// Get a reference to the connection pool
    pub fn pool(&self) -> &DhtPool {
        &self.pool
    }

    /// Get our home relay URL
    pub fn home_relay_url(&self) -> Option<String> {
        self.pool.home_relay_url()
    }

    // ========== Outgoing: Wire Protocol ==========

    /// Send a FindNode RPC to a peer over an established connection
    ///
    /// Uses irpc for serialization/framing over the connection.
    /// Used by the actor's spawned lookup tasks and candidate verification.
    pub async fn send_find_node_on_conn(
        conn: &Connection,
        target: Id,
        requester: Option<[u8; 32]>,
        requester_relay_url: Option<String>,
    ) -> Result<FindNodeResponse, PoolError> {
        let client = irpc::Client::<RpcProtocol>::boxed(
            ExistingConnection::new(conn)
        );
        client
            .rpc(FindNode { target, requester, requester_relay_url })
            .await
            .map_err(|e| PoolError::ConnectionFailed(e.to_string()))
    }

    /// Send a FindNode RPC to a peer, establishing a connection if needed
    pub async fn send_find_node(
        &self,
        dial_info: &DialInfo,
        target: Id,
        requester: Option<[u8; 32]>,
        requester_relay_url: Option<String>,
    ) -> Result<FindNodeResponse, PoolError> {
        let conn = self.pool.get_connection(dial_info).await?;
        Self::send_find_node_on_conn(conn.connection(), target, requester, requester_relay_url).await
    }

    // ========== API: Actor Commands ==========

    /// Notify that we have verified these nodes are alive
    pub async fn nodes_seen(&self, ids: &[[u8; 32]]) -> Result<(), irpc::Error> {
        self.client.notify(NodesSeen { ids: ids.to_vec() }).await
    }

    /// Notify that these nodes are unresponsive
    pub async fn nodes_dead(&self, ids: &[[u8; 32]]) -> Result<(), irpc::Error> {
        self.client.notify(NodesDead { ids: ids.to_vec() }).await
    }

    /// Add nodes to candidates list for verification
    pub async fn add_candidates(&self, ids: &[[u8; 32]]) -> Result<(), irpc::Error> {
        self.client.notify(AddCandidates { ids: ids.to_vec() }).await
    }

    /// Find the K closest nodes to a target (iterative network lookup)
    pub async fn lookup(
        &self,
        target: Id,
        initial: Option<Vec<[u8; 32]>>,
    ) -> Result<Vec<[u8; 32]>, irpc::Error> {
        self.client.rpc(Lookup { target, initial }).await
    }

    /// Get the current routing table
    pub async fn get_routing_table(&self) -> Result<Vec<Vec<[u8; 32]>>, irpc::Error> {
        self.client.rpc(GetRoutingTable).await
    }

    /// Get relay URLs for all known nodes (for persistence)
    pub async fn get_node_relay_urls(&self) -> Result<Vec<([u8; 32], String)>, irpc::Error> {
        self.client.rpc(GetNodeRelayUrls).await
    }

    /// Set relay URLs for nodes (for restoration from database)
    pub async fn set_node_relay_urls(&self, relay_urls: Vec<([u8; 32], String)>) -> Result<(), irpc::Error> {
        self.client.notify(SetNodeRelayUrls { relay_urls }).await
    }

    /// Perform a self-lookup to populate nearby buckets
    pub async fn self_lookup(&self) -> Result<(), irpc::Error> {
        self.client.rpc(SelfLookup).await
    }

    /// Perform a random lookup to refresh distant buckets
    pub async fn random_lookup(&self) -> Result<(), irpc::Error> {
        self.client.rpc(RandomLookup).await
    }

    /// Find K closest nodes from the local routing table (no network lookup)
    pub async fn find_closest(&self, target: Id, k: Option<usize>) -> Result<Vec<[u8; 32]>, irpc::Error> {
        self.client.rpc(FindClosest { target, k }).await
    }

    /// Handle an incoming FindNode request via the actor
    pub async fn handle_find_node_request(
        &self,
        target: Id,
        requester: Option<[u8; 32]>,
        requester_relay_url: Option<String>,
    ) -> Result<Vec<NodeInfo>, irpc::Error> {
        self.client.rpc(HandleFindNodeRequest { target, requester, requester_relay_url }).await
    }

    /// Register dial info for a node (e.g., bootstrap nodes)
    pub async fn register_dial_info(&self, dial_info: DialInfo) {
        self.pool.register_dial_info(dial_info).await;
    }

    // ========== Persistence ==========

    /// Save the current routing table to the database
    pub async fn save_routing_table(&self) -> Result<(), DhtServiceError> {
        // Get current routing table from actor
        let buckets = self.get_routing_table().await
            .map_err(|e| DhtServiceError::Actor(e.to_string()))?;

        // Get relay URLs for nodes
        let relay_urls: HashMap<[u8; 32], String> = self.get_node_relay_urls().await
            .map_err(|e| DhtServiceError::Actor(e.to_string()))?
            .into_iter()
            .collect();

        let timestamp = current_timestamp();
        let mut entry_count = 0;

        // Save to database using transaction for atomicity
        let mut db_lock = self.db.lock().await;
        let tx = db_lock.transaction()
            .map_err(|e| DhtServiceError::Database(e.to_string()))?;

        clear_all_entries(&tx)
            .map_err(|e| DhtServiceError::Database(e.to_string()))?;

        for (bucket_idx, entries) in buckets.iter().enumerate() {
            for endpoint_id in entries {
                let peer = PeerInfo {
                    endpoint_id: *endpoint_id,
                    endpoint_address: None,
                    address_timestamp: None,
                    last_latency_ms: None,
                    latency_timestamp: None,
                    last_seen: timestamp,
                };
                upsert_peer(&tx, &peer)
                    .map_err(|e| DhtServiceError::Database(e.to_string()))?;

                let entry = DhtEntry {
                    endpoint_id: *endpoint_id,
                    bucket_index: bucket_idx as u8,
                    added_at: timestamp,
                    relay_url: relay_urls.get(endpoint_id).cloned(),
                };
                insert_dht_entry(&tx, &entry)
                    .map_err(|e| DhtServiceError::Database(e.to_string()))?;
                entry_count += 1;
            }
        }

        tx.commit()
            .map_err(|e| DhtServiceError::Database(e.to_string()))?;

        info!(entries = entry_count, "DHT routing table saved to database");
        Ok(())
    }
}

/// Errors from DHT service operations
#[derive(Debug)]
pub enum DhtServiceError {
    Actor(String),
    Database(String),
}

impl std::fmt::Display for DhtServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Actor(e) => write!(f, "actor error: {e}"),
            Self::Database(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for DhtServiceError {}

/// Weak reference to DhtService
///
/// Used by the actor to avoid circular ownership:
/// DhtService → irpc client → actor channel
/// actor → WeakDhtService (breaks cycle)
#[derive(Debug, Clone)]
pub struct WeakDhtService {
    inner: Weak<DhtService>,
}

impl WeakDhtService {
    /// Create a weak reference from an Arc<DhtService>
    pub fn new(service: &Arc<DhtService>) -> Self {
        Self {
            inner: Arc::downgrade(service),
        }
    }

    /// Try to upgrade to a strong reference
    pub fn upgrade(&self) -> Option<Arc<DhtService>> {
        self.inner.upgrade()
    }
}
