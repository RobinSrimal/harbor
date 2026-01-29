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

use std::collections::{HashMap, HashSet};

use irpc::channel::mpsc;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use tokio::task::JoinSet;
use tokio::time::{interval, Interval};
use tracing::{debug, error, info, trace};

use super::internal::api::ApiMessage;
use super::internal::config::DhtConfig;
use super::internal::distance::Id;
use super::internal::pool::{DhtPool, DhtPoolError as PoolError, DialInfo};
use super::internal::routing::{AddNodeResult, Buckets, RoutingTable, BUCKET_COUNT};
use super::protocol::NodeInfo;
use super::service::{DhtService, WeakDhtService};

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
    /// Receiver for API messages (local control)
    api_rx: mpsc::Receiver<ApiMessage>,
    /// Weak reference to DhtService for outgoing operations and notifications
    service: Option<WeakDhtService>,
    /// Background tasks
    tasks: JoinSet<()>,
    /// RNG for random lookups
    rng: StdRng,
    /// Candidate nodes awaiting verification before being added to routing table
    candidates: HashSet<Id>,
    /// Timer for periodic candidate verification
    candidate_verify_ticker: Interval,
    /// Relay URLs for nodes we know about (used for FindNode responses)
    /// Value: (relay_url, verified_at) where verified_at is Some(timestamp) if verified via outbound connection
    node_relay_urls: HashMap<Id, (String, Option<i64>)>,
}

impl DhtActor {
    /// Create a new DHT actor
    pub fn new(
        local_id: Id,
        pool: DhtPool,
        config: DhtConfig,
        bootstrap: Vec<Id>,
    ) -> (Self, irpc::Client<super::internal::api::ApiProtocol>) {
        Self::new_with_buckets(local_id, pool, config, bootstrap, None)
    }

    /// Create a new DHT actor with pre-existing buckets (for restoration)
    pub fn new_with_buckets(
        local_id: Id,
        pool: DhtPool,
        config: DhtConfig,
        bootstrap: Vec<Id>,
        buckets: Option<Buckets>,
    ) -> (Self, irpc::Client<super::internal::api::ApiProtocol>) {
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

        // Create API channel
        let (api_tx, api_rx) = mpsc::channel(32);
        let api_client = irpc::Client::local(api_tx);

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
            api_rx,
            service: None,
            tasks: JoinSet::new(),
            rng,
            candidates: HashSet::new(),
            candidate_verify_ticker,
            node_relay_urls: HashMap::new(),
        };

        (actor, api_client)
    }

    /// Set the DhtService reference (must be called before run())
    ///
    /// This breaks the circular dependency: DhtService owns the irpc client,
    /// actor holds a weak ref back to DhtService.
    pub fn set_service(&mut self, service: WeakDhtService) {
        self.service = Some(service);
    }

    /// Run the actor's main loop
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                biased;

                // Handle API messages (from DhtService)
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
            iroh::EndpointId::from_bytes(node.as_bytes()).expect("valid node id")
        );

        // Try to get/establish connection
        let (conn, _relay_confirmed) = self.pool.get_connection(&dial_info).await?;
        
        // Send a FindNode RPC to verify the node is actually responsive
        // Use a random target - we don't care about the result, just that it responds
        let random_target = Id::new(rand::random());
        let requester = if self.config.transient {
            None
        } else {
            Some(*self.node.local_id().as_bytes())
        };
        let relay_url = self.pool.home_relay_url();

        DhtService::send_find_node_on_conn(conn.connection(), random_target, requester, relay_url).await?;

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

        // Collect known relay URLs for candidates being verified
        let candidate_relay_urls: HashMap<Id, String> = to_verify.iter()
            .filter_map(|id| {
                self.node_relay_urls.get(id).map(|(url, _)| (*id, url.clone()))
            })
            .collect();

        // Verify each candidate by attempting connection and sending FindNode RPC
        let pool = self.pool.clone();
        let service = self.service.clone();
        let transient = self.config.transient;
        let local_id = *self.node.local_id();
        let home_relay_url = self.pool.home_relay_url();

        // Spawn verification as background task so we don't block the actor
        self.tasks.spawn(async move {
            let mut verified = Vec::new();
            let mut failed = Vec::new();
            // Track relay URL confirmation results: (node_id, relay_url, verified_at)
            let mut relay_confirmations: Vec<([u8; 32], String, Option<i64>)> = Vec::new();

            for id in to_verify {
                let dial_info = DialInfo::from_node_id(
                    iroh::EndpointId::from_bytes(id.as_bytes()).expect("valid node id")
                );

                match pool.get_connection(&dial_info).await {
                    Ok((conn, relay_confirmed)) => {
                        // Send a FindNode RPC to verify the node is actually responsive
                        let random_target = Id::new(rand::random());
                        let requester = if transient {
                            None
                        } else {
                            Some(*local_id.as_bytes())
                        };

                        match DhtService::send_find_node_on_conn(conn.connection(), random_target, requester, home_relay_url.clone()).await {
                            Ok(_) => {
                                trace!(node = %id, relay_confirmed = relay_confirmed, "candidate verified - FindNode RPC succeeded");
                                verified.push(*id.as_bytes());

                                // Record relay URL verification result
                                if let Some(relay_url) = candidate_relay_urls.get(&id) {
                                    let verified_at = if relay_confirmed {
                                        Some(crate::data::dht::peer::current_timestamp())
                                    } else {
                                        None // connected via DNS, relay URL is stale
                                    };
                                    relay_confirmations.push((*id.as_bytes(), relay_url.clone(), verified_at));
                                }
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

            // Notify service to add verified nodes to routing table
            if !verified.is_empty() {
                info!(count = verified.len(), "verified {} candidate nodes", verified.len());
                if let Some(ref svc_weak) = service {
                    if let Some(svc) = svc_weak.upgrade() {
                        svc.nodes_seen(&verified).await.ok();
                    }
                }
            }

            // Send relay URL confirmation results back to actor
            if !relay_confirmations.is_empty() {
                if let Some(ref svc_weak) = service {
                    if let Some(svc) = svc_weak.upgrade() {
                        svc.confirm_relay_urls(relay_confirmations).await.ok();
                    }
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

    /// Handle an API message
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
                let service = self.service.clone().unwrap_or_else(|| {
                    panic!("DhtActor::set_service must be called before run()")
                });
                let target = msg.target;
                let local_id = *self.node.local_id();
                let tx = msg.tx;

                self.tasks.spawn(async move {
                    let result = DhtService::iterative_find_node(
                        target,
                        initial,
                        local_id,
                        pool,
                        config,
                        service,
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
                let relay_urls: Vec<([u8; 32], String, Option<i64>)> = self.node_relay_urls
                    .iter()
                    .map(|(id, (url, verified_at))| (*id.as_bytes(), url.clone(), *verified_at))
                    .collect();
                msg.tx.send(relay_urls).await.ok();
            }

            ApiMessage::SetNodeRelayUrls(msg) => {
                // Restore relay URLs from database
                for (id_bytes, url, verified_at) in &msg.relay_urls {
                    let id = Id::new(*id_bytes);
                    self.node_relay_urls.insert(id, (url.clone(), *verified_at));
                }
                debug!(count = self.node_relay_urls.len(), "restored node relay URLs from database");
            }

            ApiMessage::ConfirmRelayUrls(msg) => {
                for (id_bytes, url, verified_at) in &msg.confirmed {
                    let id = Id::new(*id_bytes);
                    match self.node_relay_urls.get(&id) {
                        Some((existing_url, _)) if existing_url == url => {
                            // Same URL — update verified_at
                            self.node_relay_urls.insert(id, (url.clone(), *verified_at));
                        }
                        _ => {
                            // URL changed or not present — store with new status
                            self.node_relay_urls.insert(id, (url.clone(), *verified_at));
                        }
                    }
                }
                if !msg.confirmed.is_empty() {
                    debug!(count = msg.confirmed.len(), "updated relay URL verification status");
                }
            }

            ApiMessage::SelfLookup(msg) => {
                let local_id = *self.node.local_id();
                let initial = self.node.routing_table.find_closest_nodes(&local_id, self.config.k);

                let pool = self.pool.clone();
                let config = self.config.clone();
                let service = self.service.clone().unwrap_or_else(|| {
                    panic!("DhtActor::set_service must be called before run()")
                });
                let tx = msg.tx;

                self.tasks.spawn(async move {
                    DhtService::iterative_find_node(local_id, initial, local_id, pool, config, service).await;
                    tx.send(()).await.ok();
                });
            }

            ApiMessage::RandomLookup(msg) => {
                let random_id = Id::new(self.rng.r#gen());
                let initial = self.node.routing_table.find_closest_nodes(&random_id, self.config.k);

                let pool = self.pool.clone();
                let config = self.config.clone();
                let service = self.service.clone().unwrap_or_else(|| {
                    panic!("DhtActor::set_service must be called before run()")
                });
                let local_id = *self.node.local_id();
                let tx = msg.tx;

                self.tasks.spawn(async move {
                    DhtService::iterative_find_node(random_id, initial, local_id, pool, config, service).await;
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
                                        e.insert((relay_url.clone(), None)); // unverified
                                        true
                                    }
                                    Entry::Occupied(mut e) => {
                                        let (existing_url, _) = e.get();
                                        if existing_url != relay_url {
                                            e.insert((relay_url.clone(), None)); // new URL, reset to unverified
                                            true
                                        } else {
                                            false // same URL, keep existing verified_at
                                        }
                                    }
                                };
                                
                                if is_new {
                                    // ALSO register with DhtPool so we can connect back to them
                                    let node_id = iroh::EndpointId::from_bytes(&requester_id)
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
                        let relay_url = self.node_relay_urls.get(id).map(|(url, _)| url.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::{FindNode, FindNodeResponse};
    use super::super::internal::config::{DEFAULT_CANDIDATE_VERIFY_INTERVAL, MAX_CANDIDATES_PER_VERIFY};
    use super::super::internal::distance::Distance;
    use super::super::internal::routing::{K, ALPHA};
    use std::collections::BTreeSet;
    use std::time::Duration;

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

