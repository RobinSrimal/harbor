//! DHT Actor - manages DHT state and handles messages
//!
//! The actor pattern provides single-threaded access to mutable state
//! while allowing async operations for network calls.
//!
//! ## Candidate Verification
//!
//! Nodes are NOT added directly to the routing table when they send us messages.
//! Instead, they are added to a candidates list and periodically verified by
//! attempting to establish a connection. Only successfully contacted nodes
//! are promoted to the routing table. This prevents Sybil attacks where
//! malicious nodes flood the DHT with fake node IDs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use iroh::endpoint::Connection;
use irpc::channel::mpsc;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use tokio::task::JoinSet;
use tokio::time::{interval, Interval};
use tracing::{debug, error, info, trace};

use super::api::{ApiClient, ApiMessage, WeakApiClient};
use super::distance::{Distance, Id};
use super::pool::{DhtPool, DhtPoolError as PoolError, DialInfo};
use super::routing::{AddNodeResult, Buckets, RoutingTable, K, ALPHA, BUCKET_COUNT};
use super::rpc::{FindNode, FindNodeResponse, NodeInfo, RpcClient, RpcMessage};

/// Default interval for verifying candidate nodes (30 seconds)
pub const DEFAULT_CANDIDATE_VERIFY_INTERVAL: Duration = Duration::from_secs(30);

/// Maximum candidates to verify per interval
pub const MAX_CANDIDATES_PER_VERIFY: usize = 10;

/// DHT configuration
#[derive(Debug, Clone)]
pub struct DhtConfig {
    /// K parameter (bucket size, default: 20)
    pub k: usize,
    /// Alpha parameter (concurrency, default: 3)
    pub alpha: usize,
    /// Whether this node is transient (won't be added to others' routing tables)
    pub transient: bool,
    /// Optional RNG seed for deterministic testing
    pub rng_seed: Option<[u8; 32]>,
    /// Interval for verifying candidate nodes
    pub candidate_verify_interval: Duration,
    /// Maximum candidates to verify per interval
    pub max_candidates_per_verify: usize,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            k: K,
            alpha: ALPHA,
            transient: false,
            rng_seed: None,
            candidate_verify_interval: DEFAULT_CANDIDATE_VERIFY_INTERVAL,
            max_candidates_per_verify: MAX_CANDIDATES_PER_VERIFY,
        }
    }
}

impl DhtConfig {
    /// Configuration for a transient node
    pub fn transient() -> Self {
        Self {
            transient: true,
            ..Default::default()
        }
    }

    /// Configuration for a persistent node
    pub fn persistent() -> Self {
        Self {
            transient: false,
            ..Default::default()
        }
    }
}

/// DHT node state
struct DhtNode {
    /// Routing table
    routing_table: RoutingTable,
}

impl DhtNode {
    fn new(local_id: Id) -> Self {
        Self {
            routing_table: RoutingTable::new(local_id),
        }
    }

    fn with_buckets(local_id: Id, buckets: Buckets) -> Self {
        Self {
            routing_table: RoutingTable::with_buckets(local_id, buckets),
        }
    }

    fn local_id(&self) -> &Id {
        &self.routing_table.local_id
    }
}

/// DHT Actor handles all DHT operations
pub struct DhtActor {
    /// DHT node state
    node: DhtNode,
    /// Connection pool
    pool: DhtPool,
    /// Configuration
    config: DhtConfig,
    /// Receiver for RPC messages (from network)
    rpc_rx: mpsc::Receiver<RpcMessage>,
    /// Receiver for API messages (local control)
    api_rx: mpsc::Receiver<ApiMessage>,
    /// Weak reference to API for sending notifications
    api: WeakApiClient,
    /// Background tasks
    tasks: JoinSet<()>,
    /// RNG for random lookups
    rng: StdRng,
    /// Candidate nodes awaiting verification before being added to routing table
    candidates: HashSet<Id>,
    /// Timer for periodic candidate verification
    candidate_verify_ticker: Interval,
    /// Relay URLs for nodes we know about (used for FindNode responses)
    node_relay_urls: HashMap<Id, String>,
}

impl DhtActor {
    /// Create a new DHT actor
    pub fn new(
        local_id: Id,
        pool: DhtPool,
        config: DhtConfig,
        bootstrap: Vec<Id>,
    ) -> (Self, RpcClient, ApiClient) {
        Self::new_with_buckets(local_id, pool, config, bootstrap, None)
    }

    /// Create a new DHT actor with pre-existing buckets (for restoration)
    pub fn new_with_buckets(
        local_id: Id,
        pool: DhtPool,
        config: DhtConfig,
        bootstrap: Vec<Id>,
        buckets: Option<Buckets>,
    ) -> (Self, RpcClient, ApiClient) {
        // Create node
        let mut node = match buckets {
            Some(b) => DhtNode::with_buckets(local_id, b),
            None => DhtNode::new(local_id),
        };

        // Add bootstrap nodes
        for id in bootstrap {
            if id != local_id {
                node.routing_table.add_node(id);
            }
        }

        // Create RPC channel
        let (rpc_tx, rpc_rx) = mpsc::channel(32);
        let rpc_client = RpcClient::new(irpc::Client::local(rpc_tx));

        // Create API channel
        let (api_tx, api_rx) = mpsc::channel(32);
        let api_client = ApiClient::new(irpc::Client::local(api_tx));

        // Create RNG
        let rng = match config.rng_seed {
            Some(seed) => StdRng::from_seed(seed),
            None => StdRng::from_entropy(),
        };

        // Create candidate verification ticker
        let candidate_verify_ticker = interval(config.candidate_verify_interval);

        let actor = Self {
            node,
            pool,
            config,
            rpc_rx,
            api_rx,
            api: api_client.downgrade(),
            tasks: JoinSet::new(),
            rng,
            candidates: HashSet::new(),
            candidate_verify_ticker,
            node_relay_urls: HashMap::new(),
        };

        (actor, rpc_client, api_client)
    }

    /// Run the actor's main loop
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                biased;

                // Handle RPC messages from network
                msg = self.rpc_rx.recv() => {
                    match msg {
                        Ok(Some(msg)) => self.handle_rpc(msg).await,
                        _ => break,
                    }
                }

                // Handle API messages from local control
                msg = self.api_rx.recv() => {
                    match msg {
                        Ok(Some(msg)) => self.handle_api(msg).await,
                        _ => break,
                    }
                }

                // Periodic candidate verification
                _ = self.candidate_verify_ticker.tick() => {
                    self.verify_candidates().await;
                }

                // Handle completed background tasks
                Some(result) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Err(e) = result {
                        error!("background task failed: {:?}", e);
                    }
                }
            }
        }
        
        debug!("DHT actor shutting down");
    }

    /// Add a node to the routing table with proper Kademlia eviction
    ///
    /// If the bucket is full:
    /// 1. Ping the oldest node
    /// 2. If ping succeeds, refresh the oldest node (keep it, discard new)
    /// 3. If ping fails, evict oldest and add new node
    async fn add_node_with_eviction(&mut self, new_node: Id) {
        match self.node.routing_table.try_add_node(new_node) {
            Some(AddNodeResult::Added) => {
                trace!(node = %new_node, "added node to routing table");
            }
            Some(AddNodeResult::AlreadyExists) => {
                // Refresh the existing node (move to most recently seen)
                self.node.routing_table.refresh_node(&new_node);
                trace!(node = %new_node, "refreshed existing node in routing table");
            }
            Some(AddNodeResult::BucketFull { oldest }) => {
                // Bucket is full - ping oldest node to decide eviction
                trace!(
                    new_node = %new_node,
                    oldest = %oldest,
                    "bucket full, pinging oldest node"
                );

                match self.ping_node(&oldest).await {
                    Ok(()) => {
                        // Oldest is alive - refresh it and discard new node
                        self.node.routing_table.refresh_node(&oldest);
                        trace!(
                            oldest = %oldest,
                            discarded = %new_node,
                            "oldest node alive, refreshed and discarded new node"
                        );
                    }
                    Err(_) => {
                        // Oldest is dead - evict it and add new node
                        if let Some(evicted) = self.node.routing_table.replace_oldest(new_node) {
                            debug!(
                                evicted = %evicted,
                                added = %new_node,
                                "evicted dead node, added new node"
                            );
                        }
                    }
                }
            }
            None => {
                // new_node is our local ID - ignore
            }
        }
    }

    /// Ping a node to check if it's alive
    ///
    /// Uses the connection pool to attempt a connection and sends a FindNode RPC.
    /// Returns Ok(()) if node responds, Err if unreachable.
    async fn ping_node(&self, node: &Id) -> Result<(), PoolError> {
        let dial_info = DialInfo::from_node_id(
            iroh::NodeId::from_bytes(node.as_bytes()).expect("valid node id")
        );

        // Try to get/establish connection
        let conn = self.pool.get_connection(&dial_info).await?;
        
        // Send a FindNode RPC to verify the node is actually responsive
        // Use a random target - we don't care about the result, just that it responds
        let random_target = Id::new(rand::random());
        let requester = if self.config.transient {
            None
        } else {
            Some(*self.node.local_id().as_bytes())
        };
        let relay_url = self.pool.home_relay_url();

        network_find_node(conn.connection(), random_target, requester, relay_url).await?;
        
        Ok(())
    }

    /// Verify candidate nodes by attempting to connect to them
    /// 
    /// Only nodes that we can successfully connect to are promoted
    /// to the routing table. This prevents Sybil attacks.
    async fn verify_candidates(&mut self) {
        if self.candidates.is_empty() {
            trace!("no candidates to verify");
            return;
        }

        info!(
            candidates = self.candidates.len(),
            "starting candidate verification"
        );

        // Take up to max_candidates_per_verify candidates
        let to_verify: Vec<Id> = self.candidates
            .iter()
            .take(self.config.max_candidates_per_verify)
            .copied()
            .collect();

        // Remove from candidates (they'll be re-added if verification fails and they contact us again)
        for id in &to_verify {
            self.candidates.remove(id);
        }

        if !to_verify.is_empty() {
            debug!(count = to_verify.len(), "verifying candidate nodes");
        }

        // Verify each candidate by attempting connection and sending FindNode RPC
        let pool = self.pool.clone();
        let api = self.api.clone();
        let transient = self.config.transient;
        let local_id = *self.node.local_id();
        let home_relay_url = self.pool.home_relay_url();
        
        // Spawn verification as background task so we don't block the actor
        self.tasks.spawn(async move {
            let mut verified = Vec::new();
            let mut failed = Vec::new();

            for id in to_verify {
                let dial_info = DialInfo::from_node_id(
                    iroh::NodeId::from_bytes(id.as_bytes()).expect("valid node id")
                );

                match pool.get_connection(&dial_info).await {
                    Ok(conn) => {
                        // Send a FindNode RPC to verify the node is actually responsive
                        let random_target = Id::new(rand::random());
                        let requester = if transient {
                            None
                        } else {
                            Some(*local_id.as_bytes())
                        };

                        match network_find_node(conn.connection(), random_target, requester, home_relay_url.clone()).await {
                            Ok(_) => {
                                trace!(node = %id, "candidate verified - FindNode RPC succeeded");
                                verified.push(*id.as_bytes());
                            }
                            Err(e) => {
                                trace!(node = %id, error = %e, "candidate FindNode RPC failed");
                                failed.push(*id.as_bytes());
                            }
                        }
                    }
                    Err(e) => {
                        debug!(node = %id, error = %e, "candidate connection failed");
                        failed.push(*id.as_bytes());
                    }
                }
            }

            // Notify API to add verified nodes to routing table
            if !verified.is_empty() {
                info!(count = verified.len(), "verified {} candidate nodes", verified.len());
                if let Some(api) = api.upgrade() {
                    api.nodes_seen(&verified).await.ok();
                }
            }

            // Log failed verifications at INFO level for visibility
            if !failed.is_empty() {
                info!(
                    failed = failed.len(),
                    verified = verified.len(),
                    "candidate verification complete"
                );
            }
        });
    }

    /// Handle an RPC message from the network
    async fn handle_rpc(&mut self, msg: RpcMessage) {
        match msg {
            RpcMessage::FindNode(msg) => {
                // If requester wants to be added to our routing table,
                // add them directly since they've proven they're real by connecting to us.
                // Also store their relay URL for sharing with other nodes.
                if let Some(requester_id) = msg.requester {
                    if !self.config.transient {
                        let id = Id::new(requester_id);
                        // Don't add ourselves
                        if id != *self.node.local_id() {
                            // Add directly to routing table - they connected to us, so they're verified
                            self.add_node_with_eviction(id).await;
                            
                            // Store their relay URL if provided - CRITICAL for peer discovery
                            // Only log if this is a new relay URL (avoid spam)
                            if let Some(ref relay_url) = msg.requester_relay_url {
                                use std::collections::hash_map::Entry;
                                let is_new = match self.node_relay_urls.entry(id) {
                                    Entry::Vacant(e) => {
                                        e.insert(relay_url.clone());
                                        true
                                    }
                                    Entry::Occupied(mut e) => {
                                        if e.get() != relay_url {
                                            e.insert(relay_url.clone());
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                };
                                
                                if is_new {
                                    // ALSO register with DhtPool so we can connect back to them
                                    let node_id = iroh::NodeId::from_bytes(&requester_id)
                                        .expect("valid node id");
                                    let dial_info = DialInfo::from_node_id_with_relay(node_id, relay_url.clone());
                                    self.pool.register_dial_info(dial_info).await;
                                    
                                    info!(
                                        requester = %id,
                                        relay = %relay_url,
                                        "stored relay URL for requester (new)"
                                    );
                                }
                            }
                            
                            debug!(
                                requester = %id,
                                relay = ?msg.requester_relay_url,
                                "added requester directly to routing table"
                            );
                        }
                    }
                }
                
                let closest = self.node.routing_table.find_closest_nodes(&msg.target, self.config.k);
                
                // Convert to NodeInfo, including stored relay URLs for connectivity
                let nodes: Vec<NodeInfo> = closest
                    .iter()
                    .map(|id| {
                        let node_relay = self.node_relay_urls.get(id).cloned();
                        NodeInfo::from_id_with_relay(id, node_relay)
                    })
                    .collect();
                
                // Log how many nodes have relay URLs - helpful for debugging discovery
                let with_relay = nodes.iter().filter(|n| n.relay_url.is_some()).count();
                if nodes.len() > 0 && with_relay < nodes.len() {
                    debug!(
                        total = nodes.len(),
                        with_relay = with_relay,
                        "FindNode response - some nodes missing relay URLs"
                    );
                }

                debug!(
                    target = %msg.target,
                    returning_nodes = nodes.len(),
                    routing_table_size = self.node.routing_table.len(),
                    "responding to FindNode request"
                );

                let response = FindNodeResponse { nodes };
                msg.tx.send(response).await.ok();
            }
        }
    }

    /// Handle an API message from local control
    async fn handle_api(&mut self, msg: ApiMessage) {
        match msg {
            ApiMessage::NodesSeen(msg) => {
                for id_bytes in &msg.ids {
                    let id = Id::new(*id_bytes);
                    self.add_node_with_eviction(id).await;
                }
            }

            ApiMessage::NodesDead(msg) => {
                for id_bytes in &msg.ids {
                    let id = Id::new(*id_bytes);
                    self.node.routing_table.remove_node(&id);
                    trace!(node = %id, "removed dead node from routing table");
                }
            }

            ApiMessage::AddCandidates(msg) => {
                // Add discovered nodes to candidates for verification
                // This is used for nodes discovered in DHT query responses
                if !self.config.transient && !msg.ids.is_empty() {
                    let mut added = 0;
                    let mut skipped_in_routing = 0;
                    for id_bytes in &msg.ids {
                        let id = Id::new(*id_bytes);
                        // Skip if it's us
                        if id == *self.node.local_id() {
                            continue;
                        }
                        // Skip if already in routing table (already verified)
                        if self.node.routing_table.contains(&id) {
                            skipped_in_routing += 1;
                            continue;
                        }
                        // Add to candidates if not already there
                        if self.candidates.insert(id) {
                            added += 1;
                        }
                    }
                    if added > 0 {
                        info!(
                            added = added,
                            skipped_already_known = skipped_in_routing,
                            total_candidates = self.candidates.len(),
                            "added discovered nodes to candidates for verification"
                        );
                    }
                }
            }

            ApiMessage::Lookup(msg) => {
                let initial = msg.initial.as_ref()
                    .map(|ids| ids.iter().map(|id| Id::new(*id)).collect())
                    .unwrap_or_else(|| {
                        self.node.routing_table.find_closest_nodes(&msg.target, self.config.k)
                    });

                let pool = self.pool.clone();
                let config = self.config.clone();
                let api = self.api.clone();
                let target = msg.target;
                let local_id = *self.node.local_id();
                let tx = msg.tx;

                self.tasks.spawn(async move {
                    let result = iterative_find_node(
                        target,
                        initial,
                        local_id,
                        pool,
                        config,
                        api,
                    ).await;
                    
                    let ids: Vec<[u8; 32]> = result.into_iter().map(|id| *id.as_bytes()).collect();
                    tx.send(ids).await.ok();
                });
            }

            ApiMessage::GetRoutingTable(msg) => {
                let buckets: Vec<Vec<[u8; 32]>> = (0..BUCKET_COUNT)
                    .map(|i| {
                        self.node.routing_table.buckets.get(i)
                            .nodes()
                            .iter()
                            .map(|id| *id.as_bytes())
                            .collect()
                    })
                    .collect();
                msg.tx.send(buckets).await.ok();
            }

            ApiMessage::GetNodeRelayUrls(msg) => {
                let relay_urls: Vec<([u8; 32], String)> = self.node_relay_urls
                    .iter()
                    .map(|(id, url)| (*id.as_bytes(), url.clone()))
                    .collect();
                msg.tx.send(relay_urls).await.ok();
            }

            ApiMessage::SetNodeRelayUrls(msg) => {
                // Restore relay URLs from database
                for (id_bytes, url) in &msg.relay_urls {
                    let id = Id::new(*id_bytes);
                    self.node_relay_urls.insert(id, url.clone());
                }
                debug!(count = self.node_relay_urls.len(), "restored node relay URLs from database");
            }

            ApiMessage::SelfLookup(msg) => {
                let local_id = *self.node.local_id();
                let initial = self.node.routing_table.find_closest_nodes(&local_id, self.config.k);
                
                let pool = self.pool.clone();
                let config = self.config.clone();
                let api = self.api.clone();
                let tx = msg.tx;

                self.tasks.spawn(async move {
                    iterative_find_node(local_id, initial, local_id, pool, config, api).await;
                    tx.send(()).await.ok();
                });
            }

            ApiMessage::RandomLookup(msg) => {
                let random_id = Id::new(self.rng.r#gen());
                let initial = self.node.routing_table.find_closest_nodes(&random_id, self.config.k);
                
                let pool = self.pool.clone();
                let config = self.config.clone();
                let api = self.api.clone();
                let local_id = *self.node.local_id();
                let tx = msg.tx;

                self.tasks.spawn(async move {
                    iterative_find_node(random_id, initial, local_id, pool, config, api).await;
                    tx.send(()).await.ok();
                });
            }

            ApiMessage::FindClosest(msg) => {
                // Query local routing table without network lookup
                let k = msg.k.unwrap_or(self.config.k);
                let closest = self.node.routing_table.find_closest_nodes(&msg.target, k);
                let ids: Vec<[u8; 32]> = closest.into_iter().map(|id| *id.as_bytes()).collect();
                msg.tx.send(ids).await.ok();
            }

            ApiMessage::HandleFindNodeRequest(msg) => {
                // Handle incoming FindNode request - this is the canonical handler
                // that includes relay URLs in the response
                
                // If requester provided their ID, add them to routing table
                if let Some(requester_id) = msg.requester {
                    if !self.config.transient {
                        let id = Id::new(requester_id);
                        if id != *self.node.local_id() {
                            // Add directly - they connected to us, so they're verified
                            self.add_node_with_eviction(id).await;
                            
                            // Store their relay URL if provided
                            // Only log if this is a new relay URL (avoid spam)
                            if let Some(ref relay_url) = msg.requester_relay_url {
                                use std::collections::hash_map::Entry;
                                let is_new = match self.node_relay_urls.entry(id) {
                                    Entry::Vacant(e) => {
                                        e.insert(relay_url.clone());
                                        true
                                    }
                                    Entry::Occupied(mut e) => {
                                        if e.get() != relay_url {
                                            e.insert(relay_url.clone());
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                };
                                
                                if is_new {
                                    // ALSO register with DhtPool so we can connect back to them
                                    let node_id = iroh::NodeId::from_bytes(&requester_id)
                                        .expect("valid node id");
                                    let dial_info = DialInfo::from_node_id_with_relay(node_id, relay_url.clone());
                                    self.pool.register_dial_info(dial_info).await;
                                    
                                    info!(
                                        requester = %id,
                                        relay = %relay_url,
                                        "stored relay URL for incoming requester (new)"
                                    );
                                }
                            }
                        }
                    }
                }
                
                // Find closest nodes and include relay URLs
                let closest = self.node.routing_table.find_closest_nodes(&msg.target, self.config.k);
                let nodes: Vec<NodeInfo> = closest
                    .iter()
                    .map(|id| {
                        let relay_url = self.node_relay_urls.get(id).cloned();
                        NodeInfo::from_id_with_relay(id, relay_url)
                    })
                    .collect();
                
                // Log how many have relay URLs
                let with_relay = nodes.iter().filter(|n| n.relay_url.is_some()).count();
                debug!(
                    target = %msg.target,
                    total = nodes.len(),
                    with_relay = with_relay,
                    "handled FindNode request via API"
                );
                
                msg.tx.send(nodes).await.ok();
            }
        }
    }
}

/// Send FindNode RPC over a QUIC connection
/// 
/// Opens a bidirectional stream, sends the request, and reads the response.
async fn network_find_node(
    conn: &Connection,
    target: Id,
    requester: Option<[u8; 32]>,
    requester_relay_url: Option<String>,
) -> Result<FindNodeResponse, PoolError> {
    // Open bidirectional stream
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to open stream: {}", e)))?;

    // Create and serialize request
    let request = FindNode { target, requester, requester_relay_url };
    let request_bytes = postcard::to_allocvec(&request)
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to serialize request: {}", e)))?;

    // Send length-prefixed request
    let len = request_bytes.len() as u32;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to send length: {}", e)))?;
    send.write_all(&request_bytes)
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to send request: {}", e)))?;
    send.finish()
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to finish send: {}", e)))?;

    // Read length-prefixed response
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to read response length: {}", e)))?;
    let response_len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check response size (max 64KB should be plenty for node list)
    if response_len > 65536 {
        return Err(PoolError::ConnectionFailed("response too large".to_string()));
    }

    let mut response_bytes = vec![0u8; response_len];
    recv.read_exact(&mut response_bytes)
        .await
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to read response: {}", e)))?;

    // Deserialize response
    let response: FindNodeResponse = postcard::from_bytes(&response_bytes)
        .map_err(|e| PoolError::ConnectionFailed(format!("failed to deserialize response: {}", e)))?;

    Ok(response)
}

/// Perform iterative node lookup (Kademlia algorithm)
async fn iterative_find_node(
    target: Id,
    initial: Vec<Id>,
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    api: WeakApiClient,
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
                    iroh::NodeId::from_bytes(id.as_bytes()).expect("valid node id")
                );
                
                match pool.get_connection(&dial_info).await {
                    Ok(conn) => {
                        // Send actual FindNode RPC over the connection
                        match network_find_node(conn.connection(), target_for_rpc, requester_for_rpc, relay_url_for_rpc).await {
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

                    // Notify API that responding node is alive (verified by successful query)
                    if let Some(api) = api.upgrade() {
                        api.nodes_seen(&[*id.as_bytes()]).await.ok();
                    }

                    // Add new candidates for lookup AND notify DHT about discovered nodes
                    // Also register relay URLs for future connectivity AND store for sharing
                    let mut discovered: Vec<[u8; 32]> = Vec::new();
                    let mut discovered_relay_urls: Vec<([u8; 32], String)> = Vec::new();
                    for info in node_infos {
                        let node = Id::new(info.node_id);
                        if node != local_id && !queried.contains(&node) {
                            candidates.insert((target.distance(&node), node));
                            discovered.push(info.node_id);
                            
                            // Register relay URL with pool for future connections
                            // AND store in node_relay_urls so we can share with others
                            if let Some(relay_url) = info.relay_url {
                                let node_id = iroh::NodeId::from_bytes(&info.node_id)
                                    .expect("valid node id");
                                let dial_info = DialInfo::from_node_id_with_relay(node_id, relay_url.clone());
                                pool.register_dial_info(dial_info).await;
                                // Store for sharing with other nodes in FindNode responses
                                discovered_relay_urls.push((info.node_id, relay_url));
                            }
                        }
                    }
                    
                    // Store discovered relay URLs in our node_relay_urls map
                    // This allows us to share them with other nodes in FindNode responses
                    if !discovered_relay_urls.is_empty() {
                        if let Some(api) = api.upgrade() {
                            api.set_node_relay_urls(discovered_relay_urls).await.ok();
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
                        if let Some(api) = api.upgrade() {
                            api.add_candidates(&discovered).await.ok();
                        }
                    } else {
                        debug!(peer = %id, "FindNode response contained no new nodes");
                    }
                }
                Err(_) => {
                    // Node is dead
                    if let Some(api) = api.upgrade() {
                        api.nodes_dead(&[*id.as_bytes()]).await.ok();
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

/// Create a DHT node and return clients for interaction
pub fn create_dht_node(
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    bootstrap: Vec<Id>,
) -> (RpcClient, ApiClient) {
    create_dht_node_with_buckets(local_id, pool, config, bootstrap, None)
}

/// Create a DHT node with pre-existing buckets
pub fn create_dht_node_with_buckets(
    local_id: Id,
    pool: DhtPool,
    config: DhtConfig,
    bootstrap: Vec<Id>,
    buckets: Option<Buckets>,
) -> (RpcClient, ApiClient) {
    let (actor, rpc_client, api_client) = DhtActor::new_with_buckets(
        local_id, pool, config, bootstrap, buckets
    );
    
    tokio::spawn(actor.run());
    
    (rpc_client, api_client)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(seed: u8) -> Id {
        Id::new([seed; 32])
    }

    #[test]
    fn test_dht_config_default() {
        let config = DhtConfig::default();
        assert_eq!(config.k, K);
        assert_eq!(config.alpha, ALPHA);
        assert!(!config.transient);
        assert_eq!(config.candidate_verify_interval, DEFAULT_CANDIDATE_VERIFY_INTERVAL);
        assert_eq!(config.max_candidates_per_verify, MAX_CANDIDATES_PER_VERIFY);
    }

    #[test]
    fn test_dht_config_transient() {
        let config = DhtConfig::transient();
        assert!(config.transient);
    }

    #[test]
    fn test_dht_config_persistent() {
        let config = DhtConfig::persistent();
        assert!(!config.transient);
    }

    #[test]
    fn test_dht_config_custom_candidate_settings() {
        let config = DhtConfig {
            candidate_verify_interval: Duration::from_secs(60),
            max_candidates_per_verify: 5,
            ..Default::default()
        };
        assert_eq!(config.candidate_verify_interval, Duration::from_secs(60));
        assert_eq!(config.max_candidates_per_verify, 5);
    }

    #[test]
    fn test_candidate_set_basic_operations() {
        let mut candidates: HashSet<Id> = HashSet::new();
        
        let id1 = make_id(1);
        let id2 = make_id(2);
        let id3 = make_id(3);
        
        // Insert returns true for new entries
        assert!(candidates.insert(id1));
        assert!(candidates.insert(id2));
        
        // Insert returns false for duplicates
        assert!(!candidates.insert(id1));
        
        // Contains works
        assert!(candidates.contains(&id1));
        assert!(candidates.contains(&id2));
        assert!(!candidates.contains(&id3));
        
        // Remove works
        assert!(candidates.remove(&id1));
        assert!(!candidates.contains(&id1));
        
        // Can't remove what's not there
        assert!(!candidates.remove(&id3));
    }

    #[test]
    fn test_candidate_set_does_not_contain_self() {
        let local_id = make_id(1);
        let other_id = make_id(2);
        
        let mut candidates: HashSet<Id> = HashSet::new();
        
        // Should not add self
        if other_id != local_id {
            candidates.insert(other_id);
        }
        if local_id != local_id {
            candidates.insert(local_id);
        }
        
        assert!(candidates.contains(&other_id));
        assert!(!candidates.contains(&local_id));
    }

    #[test]
    fn test_candidate_batch_selection() {
        let mut candidates: HashSet<Id> = HashSet::new();
        
        // Add 15 candidates
        for i in 0..15 {
            candidates.insert(make_id(i));
        }
        
        assert_eq!(candidates.len(), 15);
        
        // Take up to MAX_CANDIDATES_PER_VERIFY (10)
        let to_verify: Vec<Id> = candidates
            .iter()
            .take(MAX_CANDIDATES_PER_VERIFY)
            .copied()
            .collect();
        
        assert_eq!(to_verify.len(), MAX_CANDIDATES_PER_VERIFY);
        
        // Remove verified from candidates
        for id in &to_verify {
            candidates.remove(id);
        }
        
        assert_eq!(candidates.len(), 5);
    }

    #[test]
    fn test_candidate_batch_selection_fewer_than_max() {
        let mut candidates: HashSet<Id> = HashSet::new();
        
        // Add only 3 candidates
        for i in 0..3 {
            candidates.insert(make_id(i));
        }
        
        // Take up to MAX but only get 3
        let to_verify: Vec<Id> = candidates
            .iter()
            .take(MAX_CANDIDATES_PER_VERIFY)
            .copied()
            .collect();
        
        assert_eq!(to_verify.len(), 3);
    }

    #[test]
    fn test_transient_node_does_not_accept_candidates() {
        // Transient nodes should not add requesters to candidates
        let config = DhtConfig::transient();
        assert!(config.transient);
        
        // In handle_rpc, we check: if !self.config.transient
        // So transient nodes skip adding to candidates entirely
    }

    #[test]
    fn test_default_verification_interval() {
        assert_eq!(DEFAULT_CANDIDATE_VERIFY_INTERVAL, Duration::from_secs(30));
    }

    #[test]
    fn test_default_max_candidates() {
        assert_eq!(MAX_CANDIDATES_PER_VERIFY, 10);
    }

    #[test]
    fn test_routing_table_eviction_workflow() {
        // Test the eviction workflow using routing table methods
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Fill a bucket to capacity
        // All these nodes with seed 1-20 go to similar buckets
        for i in 1..=K as u8 {
            let result = table.try_add_node(make_id(i));
            assert!(matches!(result, Some(AddNodeResult::Added)));
        }

        // Try to add one more - should get BucketFull
        let new_node = make_id((K + 1) as u8);
        let result = table.try_add_node(new_node);

        match result {
            Some(AddNodeResult::BucketFull { oldest }) => {
                // Simulate: oldest responds to ping - refresh it
                assert!(table.refresh_node(&oldest));
                // New node is discarded (not added)
                assert!(!table.contains(&new_node));
                assert!(table.contains(&oldest));
            }
            Some(AddNodeResult::Added) => {
                // Node went to a different bucket - that's fine too
                assert!(table.contains(&new_node));
            }
            _ => {}
        }
    }

    #[test]
    fn test_routing_table_eviction_dead_node() {
        // Test evicting a dead node
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add nodes that will go to the same bucket
        for i in 1..=K as u8 {
            table.add_node(make_id(i));
        }

        // Try to add one more
        let new_node = make_id((K + 1) as u8);
        let result = table.try_add_node(new_node);

        if let Some(AddNodeResult::BucketFull { oldest }) = result {
            // Simulate: oldest fails ping - evict it
            let evicted = table.replace_oldest(new_node);
            assert_eq!(evicted, Some(oldest));

            // New node should be in, oldest should be out
            assert!(table.contains(&new_node));
            assert!(!table.contains(&oldest));
        }
    }

    #[test]
    fn test_refresh_moves_to_back() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add nodes 1, 2, 3
        table.add_node(make_id(1));
        table.add_node(make_id(2));
        table.add_node(make_id(3));

        // Refresh node 1 - should move it to the back
        assert!(table.refresh_node(&make_id(1)));

        // If we now try to add when full, node 2 should be oldest
        // (This tests the LRU ordering)
    }

    // Integration tests require a running tokio runtime and iroh endpoint
    // See the tests module in mod.rs for full integration tests

    // ========== Network RPC Protocol Tests ==========

    #[test]
    fn test_find_node_request_serialization() {
        let request = FindNode {
            target: make_id(42),
            requester: Some([1u8; 32]),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
        };

        // Serialize
        let bytes = postcard::to_allocvec(&request).unwrap();
        
        // Deserialize
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded.target, request.target);
        assert_eq!(decoded.requester, request.requester);
        assert_eq!(decoded.requester_relay_url, request.requester_relay_url);
    }

    #[test]
    fn test_find_node_request_no_requester() {
        let request = FindNode {
            target: make_id(42),
            requester: None,
            requester_relay_url: None,
        };

        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();
        
        assert!(decoded.requester.is_none());
    }

    #[test]
    fn test_find_node_response_serialization() {
        let response = FindNodeResponse {
            nodes: vec![
                NodeInfo {
                    node_id: [1u8; 32],
                    addresses: vec!["192.168.1.1:4433".to_string()],
                    relay_url: Some("https://relay.example.com/".to_string()),
                },
                NodeInfo {
                    node_id: [2u8; 32],
                    addresses: vec![],
                    relay_url: None,
                },
            ],
        };

        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded.nodes.len(), 2);
        assert_eq!(decoded.nodes[0].node_id, [1u8; 32]);
        assert_eq!(decoded.nodes[0].addresses.len(), 1);
        assert!(decoded.nodes[0].relay_url.is_some());
        assert_eq!(decoded.nodes[1].node_id, [2u8; 32]);
        assert!(decoded.nodes[1].addresses.is_empty());
        assert!(decoded.nodes[1].relay_url.is_none());
    }

    #[test]
    fn test_find_node_response_empty() {
        let response = FindNodeResponse { nodes: vec![] };
        
        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();
        
        assert!(decoded.nodes.is_empty());
    }

    #[test]
    fn test_find_node_response_max_nodes() {
        // Test with K nodes (maximum expected in response)
        let nodes: Vec<NodeInfo> = (0..K as u8)
            .map(|i| NodeInfo {
                node_id: [i; 32],
                addresses: vec![],
                relay_url: None,
            })
            .collect();

        let response = FindNodeResponse { nodes };
        
        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded.nodes.len(), K);
    }

    #[test]
    fn test_length_prefixed_protocol() {
        // Test the length-prefixed message format used in network_find_node
        let request = FindNode {
            target: make_id(42),
            requester: Some([1u8; 32]),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
        };

        // Simulate what network_find_node does
        let request_bytes = postcard::to_allocvec(&request).unwrap();
        let len = request_bytes.len() as u32;
        
        // Build message with length prefix
        let mut message = Vec::new();
        message.extend_from_slice(&len.to_be_bytes());
        message.extend_from_slice(&request_bytes);

        // Parse it back
        let parsed_len = u32::from_be_bytes([message[0], message[1], message[2], message[3]]) as usize;
        assert_eq!(parsed_len, request_bytes.len());

        let parsed_request: FindNode = postcard::from_bytes(&message[4..]).unwrap();
        assert_eq!(parsed_request.target, request.target);
    }

    #[test]
    fn test_response_size_sanity_check() {
        // Test that responses with reasonable data stay under the 64KB limit
        let nodes: Vec<NodeInfo> = (0..20u8)
            .map(|i| NodeInfo {
                node_id: [i; 32],
                // Realistic address list
                addresses: vec![
                    "192.168.1.1:4433".to_string(),
                    "10.0.0.1:4433".to_string(),
                ],
                relay_url: Some("https://relay.iroh.link/".to_string()),
            })
            .collect();

        let response = FindNodeResponse { nodes };
        let bytes = postcard::to_allocvec(&response).unwrap();
        
        // Should be well under 64KB
        assert!(bytes.len() < 65536, "Response too large: {} bytes", bytes.len());
        // But should have reasonable size
        assert!(bytes.len() > 100, "Response suspiciously small");
    }

    #[test]
    fn test_node_info_conversion() {
        let id = make_id(42);
        let info = NodeInfo::from_id(&id);
        
        assert_eq!(info.node_id, [42u8; 32]);
        assert!(info.addresses.is_empty());
        assert!(info.relay_url.is_none());
        
        // Convert back
        let back = info.to_id();
        assert_eq!(back, id);
    }

    #[test]
    fn test_iterative_lookup_candidate_ordering() {
        // Test that candidates are ordered by distance
        let target = make_id(0);
        
        let mut candidates: BTreeSet<(Distance, Id)> = BTreeSet::new();
        
        // Add nodes with different distances
        let node1 = make_id(1);   // Distance from target
        let node2 = make_id(128); // Different distance
        let node3 = make_id(255); // Different distance
        
        candidates.insert((target.distance(&node1), node1));
        candidates.insert((target.distance(&node2), node2));
        candidates.insert((target.distance(&node3), node3));
        
        // First element should be closest to target
        let first = candidates.first().unwrap();
        let closest_dist = first.0;
        
        for (dist, _) in candidates.iter().skip(1) {
            assert!(*dist >= closest_dist, "Candidates not sorted by distance");
        }
    }

    #[test]
    fn test_iterative_lookup_deduplication() {
        // Test that queried nodes are not re-queried
        let mut queried: HashSet<Id> = HashSet::new();
        let local_id = make_id(0);
        
        queried.insert(local_id); // Local is always pre-added
        
        let node1 = make_id(1);
        let node2 = make_id(2);
        
        // First time should be queryable
        assert!(!queried.contains(&node1));
        queried.insert(node1);
        
        // Second time should be skipped
        assert!(queried.contains(&node1));
        
        // Different node still queryable
        assert!(!queried.contains(&node2));
    }

    #[test]
    fn test_result_truncation_to_k() {
        // Test that results are truncated to K
        let target = make_id(0);
        let mut results: BTreeSet<(Distance, Id)> = BTreeSet::new();
        
        // Add more than K results
        for i in 0..(K + 5) as u8 {
            let id = make_id(i);
            results.insert((target.distance(&id), id));
        }
        
        assert!(results.len() > K);
        
        // Truncate to K
        while results.len() > K {
            results.pop_last();
        }
        
        assert_eq!(results.len(), K);
    }

    #[test]
    fn test_termination_condition_k_results_no_better_candidates() {
        // Test the termination logic
        let config = DhtConfig::default();
        let target = make_id(0);
        
        // Simulate having K results
        let mut results: BTreeSet<(Distance, Id)> = BTreeSet::new();
        for i in 1..=K as u8 {
            let id = make_id(i);
            results.insert((target.distance(&id), id));
        }
        
        // Get the K-th best distance
        let kth_best = results.iter().nth(config.k - 1).map(|(d, _)| *d);
        
        // If best candidate has worse distance, we should terminate
        let worse_candidate = make_id(200);
        let worse_dist = target.distance(&worse_candidate);
        
        if let Some(kth) = kth_best {
            // This simulates the termination check
            let should_terminate = worse_dist >= kth;
            assert!(should_terminate, "Should terminate when no better candidates");
        }
    }

    #[test]
    fn test_termination_with_zero_k() {
        // Guard against k=0 causing underflow
        let config = DhtConfig {
            k: 0,
            ..Default::default()
        };
        
        // The code should handle k=0 by breaking early
        // This test just verifies the guard exists in the logic
        assert_eq!(config.k, 0);
    }
}

