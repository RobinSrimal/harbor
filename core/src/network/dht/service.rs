//! DHT Service - single entry point for all DHT operations
//!
//! Wraps the DHT actor, connection pool, and provides both
//! incoming and outgoing DHT protocol handling.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex, Weak};

use iroh::endpoint::Connection;
use iroh::Endpoint;
use rusqlite::Connection as DbConnection;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tracing::{debug, info, trace};

use super::internal::api::{
    ApiProtocol, AddCandidates, ConfirmRelayUrls, FindClosest, GetNodeRelayUrls,
    GetRoutingTable, HandleFindNodeRequest, Lookup, NodesDead, NodesSeen, RandomLookup,
    SelfLookup, SetNodeRelayUrls,
};
use super::internal::bootstrap::{bootstrap_dial_info, bootstrap_node_ids};
use super::internal::config::{create_dht_actor_with_buckets, DhtConfig};
use super::internal::distance::{Distance, Id};
use super::internal::pool::{DhtPool, DhtPoolConfig, DhtPoolError as PoolError, DialInfo, DHT_ALPN};
use super::internal::routing::Buckets;
use super::protocol::{FindNode, FindNodeResponse, NodeInfo, RpcProtocol};
use crate::data::dht::{clear_all_entries, current_timestamp, insert_dht_entry, store_peer_relay_url_unverified, update_peer_relay_url, upsert_peer, DhtEntry, PeerInfo};
use crate::network::connect::Connector;
use crate::network::rpc::ExistingConnection;
use crate::resilience::{ProofOfWork, PoWConfig, PoWResult, PoWVerifier, build_context};

/// DHT Service - the single entry point for all DHT operations
///
/// Owns the connection pool and actor channel. All DHT operations
/// (incoming handlers, outgoing RPCs, lookups) go through this service.
impl std::fmt::Debug for DhtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtService").finish_non_exhaustive()
    }
}

pub struct DhtService {
    /// Connection pool for DHT peers
    pool: DhtPool,
    /// irpc client for sending commands to the actor
    client: irpc::Client<ApiProtocol>,
    /// Database connection for persistence
    db: Arc<Mutex<DbConnection>>,
    /// Our endpoint ID (for filtering self from DHT)
    our_id: [u8; 32],
    /// Proof of Work verifier for abuse prevention
    pow_verifier: StdMutex<PoWVerifier>,
}

impl DhtService {
    /// Create a new DHT service
    pub fn new(pool: DhtPool, client: irpc::Client<ApiProtocol>, db: Arc<Mutex<DbConnection>>, our_id: [u8; 32]) -> Arc<Self> {
        Arc::new(Self {
            pool,
            client,
            db,
            our_id,
            pow_verifier: StdMutex::new(PoWVerifier::new(PoWConfig::dht())),
        })
    }

    /// Initialize and start the DHT service, including pool, actor, and bootstrap registration.
    ///
    /// This encapsulates all DHT startup logic:
    /// 1. Creates the connection pool
    /// 2. Parses and registers bootstrap nodes
    /// 3. Loads persisted routing table from DB
    /// 4. Creates and spawns the DHT actor
    /// 5. Restores relay URLs from DB
    pub async fn start(
        endpoint: Endpoint,
        bootstrap_nodes: &[String],
        db: Arc<Mutex<DbConnection>>,
        our_id: [u8; 32],
    ) -> Arc<Self> {
        let local_dht_id = Id::new(our_id);
        let pool_config = DhtPoolConfig::default();
        let dht_connector = Arc::new(Connector::new(
            endpoint.clone(),
            db.clone(),
            DHT_ALPN,
            pool_config.connect_timeout,
            Some(pool_config),
        ));
        let dht_pool = DhtPool::new(dht_connector);

        // Parse and register bootstrap nodes from config
        let mut bootstrap_ids: Vec<Id> = Vec::new();

        for bootstrap_str in bootstrap_nodes {
            let (endpoint_hex, relay_url) = if bootstrap_str.contains(':') {
                if let Some(colon_pos) = bootstrap_str.find(':') {
                    let first_part = &bootstrap_str[..colon_pos];
                    if first_part.len() == 64 && first_part.chars().all(|c| c.is_ascii_hexdigit()) {
                        (
                            first_part.to_string(),
                            Some(bootstrap_str[colon_pos + 1..].to_string()),
                        )
                    } else {
                        (bootstrap_str.clone(), None)
                    }
                } else {
                    (bootstrap_str.clone(), None)
                }
            } else {
                (bootstrap_str.clone(), None)
            };

            if let Ok(bytes) = hex::decode(&endpoint_hex) {
                if bytes.len() == 32 {
                    let mut id_bytes = [0u8; 32];
                    id_bytes.copy_from_slice(&bytes);

                    if let Ok(node_id) = iroh::EndpointId::from_bytes(&id_bytes) {
                        let dial_info = if let Some(ref relay) = relay_url {
                            DialInfo::from_node_id_with_relay(node_id, relay.clone())
                        } else {
                            DialInfo::from_node_id(node_id)
                        };
                        dht_pool.register_dial_info(dial_info).await;
                        bootstrap_ids.push(Id::new(id_bytes));

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
                .map(Id::new)
                .collect();
        }

        // Load persisted routing table and relay URLs from database
        let (persisted_buckets, persisted_relay_urls): (Option<Buckets>, Vec<([u8; 32], String, Option<i64>)>) = {
            let db_lock = db.lock().await;
            match crate::data::dht::load_routing_table(&db_lock) {
                Ok(bucket_vecs) => {
                    let mut buckets = Buckets::new();
                    let mut loaded_count = 0;
                    for (idx, entries) in bucket_vecs.into_iter().enumerate() {
                        if idx < super::BUCKET_COUNT {
                            for id_bytes in entries {
                                buckets
                                    .get_mut(idx)
                                    .add_node(Id::new(id_bytes));
                                loaded_count += 1;
                            }
                        }
                    }
                    // Load relay URLs from peers table
                    let relay_urls = crate::data::dht::load_routing_table_relay_urls(&db_lock)
                        .unwrap_or_default();
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
            }
        };

        let bootstrap_count = bootstrap_ids.len();

        let service_pool = dht_pool.clone();
        let (mut dht_actor, dht_api_client) = create_dht_actor_with_buckets(
            local_dht_id,
            dht_pool,
            DhtConfig::persistent(),
            bootstrap_ids,
            persisted_buckets,
        );

        let dht_service = Self::new(
            service_pool,
            dht_api_client,
            db,
            our_id,
        );
        dht_actor.set_service(WeakDhtService::new(&dht_service));
        tokio::spawn(dht_actor.run());

        // Restore relay URLs from database
        if !persisted_relay_urls.is_empty() {
            if let Err(e) = dht_service.set_node_relay_urls(persisted_relay_urls).await {
                tracing::warn!(error = %e, "Failed to restore relay URLs from database");
            }
        }

        info!(
            bootstrap_count = bootstrap_count,
            "DHT initialized with relay info"
        );

        dht_service
    }

    /// Get a reference to the database connection
    pub(crate) fn db(&self) -> &Arc<Mutex<DbConnection>> {
        &self.db
    }

    /// Get our endpoint ID
    pub(crate) fn our_id(&self) -> [u8; 32] {
        self.our_id
    }

    /// Get a reference to the connection pool
    pub fn pool(&self) -> &DhtPool {
        &self.pool
    }

    /// Get our home relay URL
    pub fn home_relay_url(&self) -> Option<String> {
        self.pool.home_relay_url()
    }

    // ========== PoW Verification ==========

    /// Verify Proof of Work for a DHT request
    ///
    /// Context for DHT PoW: `sender_id || target_bytes`
    /// Uses RequestBased scaling.
    pub fn verify_pow(
        &self,
        pow: &ProofOfWork,
        sender_id: &[u8; 32],
        target: &Id,
    ) -> bool {
        let context = build_context(&[sender_id, target.as_bytes()]);
        let verifier = self.pow_verifier.lock().unwrap();

        matches!(
            verifier.verify(pow, &context, sender_id, None),
            PoWResult::Allowed
        )
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
        // Compute PoW (context: sender_id || target_bytes)
        let sender_id = requester.unwrap_or([0u8; 32]);
        let pow_context = build_context(&[&sender_id, target.as_bytes()]);
        let pow = ProofOfWork::compute(&pow_context, PoWConfig::dht().base_difficulty)
            .ok_or_else(|| PoolError::ConnectionFailed("failed to compute PoW".to_string()))?;

        let client = irpc::Client::<RpcProtocol>::boxed(
            ExistingConnection::new(conn)
        );
        client
            .rpc(FindNode { target, requester, requester_relay_url, pow })
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
        let (conn, _relay_confirmed) = self.pool.get_connection(dial_info).await?;
        Self::send_find_node_on_conn(&conn, target, requester, requester_relay_url).await
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
    pub async fn get_node_relay_urls(&self) -> Result<Vec<([u8; 32], String, Option<i64>)>, irpc::Error> {
        self.client.rpc(GetNodeRelayUrls).await
    }

    /// Set relay URLs for nodes (for restoration from database)
    pub async fn set_node_relay_urls(&self, relay_urls: Vec<([u8; 32], String, Option<i64>)>) -> Result<(), irpc::Error> {
        self.client.notify(SetNodeRelayUrls { relay_urls }).await
    }

    /// Confirm relay URLs after outbound connection verification
    pub async fn confirm_relay_urls(&self, confirmed: Vec<([u8; 32], String, Option<i64>)>) -> Result<(), irpc::Error> {
        self.client.notify(ConfirmRelayUrls { confirmed }).await
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

        // Get relay URLs for nodes (with verification timestamps)
        let relay_urls: HashMap<[u8; 32], (String, Option<i64>)> = self.get_node_relay_urls().await
            .map_err(|e| DhtServiceError::Actor(e.to_string()))?
            .into_iter()
            .map(|(id, url, verified_at)| (id, (url, verified_at)))
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
                    last_latency_ms: None,
                    latency_timestamp: None,
                    last_seen: timestamp,
                    relay_url: None,
                    relay_url_last_success: None,
                };
                upsert_peer(&tx, &peer)
                    .map_err(|e| DhtServiceError::Database(e.to_string()))?;

                let entry = DhtEntry {
                    endpoint_id: *endpoint_id,
                    bucket_index: bucket_idx as u8,
                    added_at: timestamp,
                };
                insert_dht_entry(&tx, &entry)
                    .map_err(|e| DhtServiceError::Database(e.to_string()))?;

                // Write relay URL to peers table if known
                if let Some((relay_url, verified_at)) = relay_urls.get(endpoint_id) {
                    match verified_at {
                        Some(ts) => {
                            update_peer_relay_url(&tx, endpoint_id, relay_url, *ts)
                                .map_err(|e| DhtServiceError::Database(e.to_string()))?;
                        }
                        None => {
                            store_peer_relay_url_unverified(&tx, endpoint_id, relay_url)
                                .map_err(|e| DhtServiceError::Database(e.to_string()))?;
                        }
                    }
                }

                entry_count += 1;
            }
        }

        tx.commit()
            .map_err(|e| DhtServiceError::Database(e.to_string()))?;

        info!(entries = entry_count, "DHT routing table saved to database");
        Ok(())
    }

    /// Perform iterative node lookup (Kademlia algorithm)
    ///
    /// Starting from an initial set of nodes, iteratively queries nodes
    /// for closer peers until convergence. Returns the K closest nodes found.
    ///
    /// This is a static method because it's called from spawned actor tasks
    /// that hold a WeakDhtService reference (to break ownership cycles).
    pub async fn iterative_find_node(
        target: Id,
        initial: Vec<Id>,
        local_id: Id,
        pool: DhtPool,
        config: DhtConfig,
        service: WeakDhtService,
    ) -> Vec<Id> {
        // Track candidates sorted by distance
        let mut candidates: BTreeSet<(Distance, Id)> = initial
            .into_iter()
            .filter(|id| *id != local_id)
            .map(|id| (target.distance(&id), id))
            .collect();

        // Track which nodes we've queried
        let mut queried: HashSet<Id> = HashSet::new();
        queried.insert(local_id);

        // Track results
        let mut results: BTreeSet<(Distance, Id)> = BTreeSet::new();
        results.insert((target.distance(&local_id), local_id));

        loop {
            // Get alpha unqueried candidates to query
            let to_query: Vec<Id> = candidates
                .iter()
                .filter(|(_, id)| !queried.contains(id))
                .take(config.alpha)
                .map(|(_, id)| *id)
                .collect();

            if to_query.is_empty() {
                break;
            }

            // Mark as queried
            for id in &to_query {
                queried.insert(*id);
            }

            // Get our relay URL to include in requests
            let home_relay_url = pool.home_relay_url();

            // Query nodes in parallel
            let mut query_tasks = JoinSet::new();
            for id in to_query {
                let pool = pool.clone();
                let target_for_rpc = target;
                let requester_for_rpc = if config.transient {
                    None
                } else {
                    Some(*local_id.as_bytes())
                };
                let relay_url_for_rpc = home_relay_url.clone();

                query_tasks.spawn(async move {
                    // Create dial info - pool will merge with known relay URLs
                    let dial_info = DialInfo::from_node_id(
                        iroh::EndpointId::from_bytes(id.as_bytes()).expect("valid node id")
                    );

                    match pool.get_connection(&dial_info).await {
                        Ok((conn, _relay_confirmed)) => {
                            // Send actual FindNode RPC over the connection
                            match DhtService::send_find_node_on_conn(&conn, target_for_rpc, requester_for_rpc, relay_url_for_rpc).await {
                                Ok(response) => {
                                    // Return full NodeInfo so caller can extract relay URLs
                                    trace!(
                                        peer = %id,
                                        nodes_returned = response.nodes.len(),
                                        "FindNode RPC succeeded"
                                    );
                                    (id, Ok(response.nodes))
                                }
                                Err(e) => {
                                    debug!(peer = %id, error = %e, "FindNode RPC failed");
                                    (id, Err(e))
                                }
                            }
                        }
                        Err(e) => {
                            debug!(peer = %id, error = %e, "Failed to connect for FindNode");
                            (id, Err(e))
                        }
                    }
                });
            }

            // Collect results
            while let Some(result) = query_tasks.join_next().await {
                let Ok((id, query_result)) = result else {
                    continue;
                };

                match query_result {
                    Ok(node_infos) => {
                        // Add to results
                        let dist = target.distance(&id);
                        results.insert((dist, id));

                        // Notify service that responding node is alive (verified by successful query)
                        if let Some(svc) = service.upgrade() {
                            svc.nodes_seen(&[*id.as_bytes()]).await.ok();
                        }

                        // Add new candidates for lookup AND notify DHT about discovered nodes
                        // Also register relay URLs for future connectivity AND store for sharing
                        let mut discovered: Vec<[u8; 32]> = Vec::new();
                        let mut discovered_relay_urls: Vec<([u8; 32], String, Option<i64>)> = Vec::new();
                        for info in node_infos {
                            let node = Id::new(info.node_id);
                            if node != local_id && !queried.contains(&node) {
                                candidates.insert((target.distance(&node), node));
                                discovered.push(info.node_id);

                                // Register relay URL with pool for future connections
                                // AND store in node_relay_urls so we can share with others
                                if let Some(relay_url) = info.relay_url {
                                    let node_id = iroh::EndpointId::from_bytes(&info.node_id)
                                        .expect("valid node id");
                                    let dial_info = DialInfo::from_node_id_with_relay(node_id, relay_url.clone());
                                    pool.register_dial_info(dial_info).await;
                                    // Store for sharing with other nodes in FindNode responses (unverified)
                                    discovered_relay_urls.push((info.node_id, relay_url, None));
                                }
                            }
                        }

                        // Store discovered relay URLs in our node_relay_urls map
                        // This allows us to share them with other nodes in FindNode responses
                        if !discovered_relay_urls.is_empty() {
                            if let Some(svc) = service.upgrade() {
                                svc.set_node_relay_urls(discovered_relay_urls).await.ok();
                            }
                        }

                        // Add discovered nodes to DHT candidates for verification
                        // This enables proper peer discovery propagation - nodes learn
                        // about each other through query responses, not just direct contact
                        if !discovered.is_empty() {
                            debug!(
                                peer = %id,
                                discovered_count = discovered.len(),
                                "discovered nodes from FindNode response"
                            );
                            if let Some(svc) = service.upgrade() {
                                svc.add_candidates(&discovered).await.ok();
                            }
                        } else {
                            debug!(peer = %id, "FindNode response contained no new nodes");
                        }
                    }
                    Err(_) => {
                        // Node is dead
                        if let Some(svc) = service.upgrade() {
                            svc.nodes_dead(&[*id.as_bytes()]).await.ok();
                        }
                    }
                }
            }

            // Truncate results to K
            while results.len() > config.k {
                results.pop_last();
            }

            // Check if we have K results and no better candidates
            // Guard against k=0 (would cause underflow)
            if config.k == 0 {
                break;
            }
            let kth_best = results.iter().nth(config.k - 1).map(|(d, _)| *d);
            let best_candidate = candidates.first().map(|(d, _)| *d);

            match (kth_best, best_candidate) {
                (Some(kth), Some(best)) if best >= kth => break,
                (Some(_), None) => break,
                _ => continue,
            }
        }

        results.into_iter().map(|(_, id)| id).collect()
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
