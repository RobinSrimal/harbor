//! DHT high-level API
//!
//! Provides operations that affect the entire DHT network:
//! - Lookup: Find the K closest nodes to a target
//! - NodesSeen: Update routing table when nodes are verified
//! - NodesDead: Remove unresponsive nodes from routing table

use std::sync::Arc;

use irpc::channel::{oneshot, none::NoSender};
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

use super::distance::Id;
use super::rpc::NodeInfo;

/// DHT API protocol definition
#[rpc_requests(message = ApiMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum ApiProtocol {
    /// Notify that we have verified these nodes are alive
    #[rpc(tx = NoSender)]
    #[wrap(NodesSeen)]
    NodesSeen { ids: Vec<[u8; 32]> },

    /// Notify that these nodes are unresponsive
    #[rpc(tx = NoSender)]
    #[wrap(NodesDead)]
    NodesDead { ids: Vec<[u8; 32]> },

    /// Add nodes to candidates list for verification
    /// Use this for discovered nodes that haven't been directly verified yet
    #[rpc(tx = NoSender)]
    #[wrap(AddCandidates)]
    AddCandidates { ids: Vec<[u8; 32]> },

    /// Perform iterative lookup to find K closest nodes to target
    #[rpc(tx = oneshot::Sender<Vec<[u8; 32]>>)]
    #[wrap(Lookup)]
    Lookup {
        /// Target ID to find closest nodes for
        target: Id,
        /// Optional initial nodes to start lookup from
        initial: Option<Vec<[u8; 32]>>,
    },

    /// Get the current routing table (for debugging/stats)
    #[rpc(tx = oneshot::Sender<Vec<Vec<[u8; 32]>>>)]
    #[wrap(GetRoutingTable)]
    GetRoutingTable,

    /// Get node relay URLs (for persistence)
    #[rpc(tx = oneshot::Sender<Vec<([u8; 32], String)>>)]
    #[wrap(GetNodeRelayUrls)]
    GetNodeRelayUrls,

    /// Set node relay URLs (for restoration from database)
    #[rpc(tx = NoSender)]
    #[wrap(SetNodeRelayUrls)]
    SetNodeRelayUrls {
        /// List of (node_id, relay_url) pairs
        relay_urls: Vec<([u8; 32], String)>,
    },

    /// Perform a self-lookup to populate nearby buckets
    #[rpc(tx = oneshot::Sender<()>)]
    #[wrap(SelfLookup)]
    SelfLookup,

    /// Perform a random lookup to refresh distant buckets
    #[rpc(tx = oneshot::Sender<()>)]
    #[wrap(RandomLookup)]
    RandomLookup,

    /// Find K closest nodes to a target from local routing table (no network lookup)
    #[rpc(tx = oneshot::Sender<Vec<[u8; 32]>>)]
    #[wrap(FindClosest)]
    FindClosest {
        /// Target ID to find closest nodes for
        target: Id,
        /// Maximum number of nodes to return (defaults to K if not specified)
        k: Option<usize>,
    },

    /// Handle an incoming FindNode request (returns NodeInfo with relay URLs)
    /// 
    /// This also handles adding the requester to the routing table if they provided their ID.
    #[rpc(tx = oneshot::Sender<Vec<NodeInfo>>)]
    #[wrap(HandleFindNodeRequest)]
    HandleFindNodeRequest {
        /// Target ID to find closest nodes for
        target: Id,
        /// Requester's node ID (if they want to be added to routing table)
        requester: Option<[u8; 32]>,
        /// Requester's relay URL (for connectivity)
        requester_relay_url: Option<String>,
    },
}

/// DHT API client
#[derive(Debug, Clone)]
pub struct ApiClient {
    inner: Arc<irpc::Client<ApiProtocol>>,
}

impl ApiClient {
    /// Create a new API client
    pub fn new(client: irpc::Client<ApiProtocol>) -> Self {
        Self {
            inner: Arc::new(client),
        }
    }

    /// Notify that we have verified these nodes are alive
    pub async fn nodes_seen(&self, ids: &[[u8; 32]]) -> Result<(), irpc::Error> {
        self.inner.notify(NodesSeen { ids: ids.to_vec() }).await
    }

    /// Notify that these nodes are unresponsive
    pub async fn nodes_dead(&self, ids: &[[u8; 32]]) -> Result<(), irpc::Error> {
        self.inner.notify(NodesDead { ids: ids.to_vec() }).await
    }

    /// Add nodes to candidates list for verification
    /// 
    /// Use this for discovered nodes (e.g., from DHT query responses) that
    /// haven't been directly verified yet. They will be verified before
    /// being added to the routing table.
    pub async fn add_candidates(&self, ids: &[[u8; 32]]) -> Result<(), irpc::Error> {
        self.inner.notify(AddCandidates { ids: ids.to_vec() }).await
    }

    /// Find the K closest nodes to a target
    pub async fn lookup(
        &self,
        target: Id,
        initial: Option<Vec<[u8; 32]>>,
    ) -> Result<Vec<[u8; 32]>, irpc::Error> {
        self.inner.rpc(Lookup { target, initial }).await
    }

    /// Get the current routing table
    pub async fn get_routing_table(&self) -> Result<Vec<Vec<[u8; 32]>>, irpc::Error> {
        self.inner.rpc(GetRoutingTable).await
    }

    /// Get relay URLs for all known nodes (for persistence)
    pub async fn get_node_relay_urls(&self) -> Result<Vec<([u8; 32], String)>, irpc::Error> {
        self.inner.rpc(GetNodeRelayUrls).await
    }

    /// Set relay URLs for nodes (for restoration from database)
    pub async fn set_node_relay_urls(&self, relay_urls: Vec<([u8; 32], String)>) -> Result<(), irpc::Error> {
        self.inner.notify(SetNodeRelayUrls { relay_urls }).await
    }

    /// Perform a self-lookup to populate nearby buckets
    pub async fn self_lookup(&self) -> Result<(), irpc::Error> {
        self.inner.rpc(SelfLookup).await
    }

    /// Perform a random lookup to refresh distant buckets
    pub async fn random_lookup(&self) -> Result<(), irpc::Error> {
        self.inner.rpc(RandomLookup).await
    }

    /// Find K closest nodes to a target from the local routing table (no network lookup)
    /// 
    /// This is a synchronous query of the in-memory routing table, unlike `lookup()`
    /// which performs an iterative network lookup.
    pub async fn find_closest(&self, target: Id, k: Option<usize>) -> Result<Vec<[u8; 32]>, irpc::Error> {
        self.inner.rpc(FindClosest { target, k }).await
    }

    /// Handle an incoming FindNode request
    /// 
    /// This returns NodeInfo with relay URLs (unlike find_closest which only returns IDs).
    /// Also handles adding the requester to the routing table if they provided their ID.
    pub async fn handle_find_node_request(
        &self,
        target: Id,
        requester: Option<[u8; 32]>,
        requester_relay_url: Option<String>,
    ) -> Result<Vec<NodeInfo>, irpc::Error> {
        self.inner.rpc(HandleFindNodeRequest { target, requester, requester_relay_url }).await
    }

    /// Create a weak reference
    pub fn downgrade(&self) -> WeakApiClient {
        WeakApiClient {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

/// Weak reference to API client
#[derive(Debug, Clone)]
pub struct WeakApiClient {
    inner: std::sync::Weak<irpc::Client<ApiProtocol>>,
}

impl WeakApiClient {
    /// Try to upgrade to a strong reference
    pub fn upgrade(&self) -> Option<ApiClient> {
        self.inner.upgrade().map(|inner| ApiClient { inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(seed: u8) -> Id {
        Id::new([seed; 32])
    }

    #[test]
    fn test_lookup_serialization() {
        let lookup = Lookup {
            target: make_id(1),
            initial: Some(vec![[2u8; 32], [3u8; 32]]),
        };

        let bytes = postcard::to_allocvec(&lookup).unwrap();
        let decoded: Lookup = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.target, lookup.target);
        assert_eq!(decoded.initial.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_nodes_seen_serialization() {
        let msg = NodesSeen {
            ids: vec![[1u8; 32], [2u8; 32]],
        };

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded: NodesSeen = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.ids.len(), 2);
    }

    #[test]
    fn test_nodes_dead_serialization() {
        let msg = NodesDead {
            ids: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
        };

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded: NodesDead = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.ids.len(), 3);
    }

    #[test]
    fn test_get_routing_table_serialization() {
        let msg = GetRoutingTable;

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let _decoded: GetRoutingTable = postcard::from_bytes(&bytes).unwrap();
        // Unit struct - just verify it round-trips
    }

    #[test]
    fn test_self_lookup_serialization() {
        let msg = SelfLookup;

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let _decoded: SelfLookup = postcard::from_bytes(&bytes).unwrap();
    }

    #[test]
    fn test_random_lookup_serialization() {
        let msg = RandomLookup;

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let _decoded: RandomLookup = postcard::from_bytes(&bytes).unwrap();
    }

    #[test]
    fn test_lookup_with_no_initial() {
        let lookup = Lookup {
            target: make_id(5),
            initial: None,
        };

        let bytes = postcard::to_allocvec(&lookup).unwrap();
        let decoded: Lookup = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.target, lookup.target);
        assert!(decoded.initial.is_none());
    }

    #[test]
    fn test_nodes_seen_empty() {
        let msg = NodesSeen { ids: vec![] };

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded: NodesSeen = postcard::from_bytes(&bytes).unwrap();

        assert!(decoded.ids.is_empty());
    }

    #[test]
    fn test_add_candidates_serialization() {
        let msg = AddCandidates {
            ids: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
        };

        let bytes = postcard::to_allocvec(&msg).unwrap();
        let decoded: AddCandidates = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.ids.len(), 3);
        assert_eq!(decoded.ids[0], [1u8; 32]);
    }

    // Note: WeakApiClient upgrade/downgrade requires a running irpc server,
    // which is tested in integration tests. The pattern itself (Arc/Weak)
    // is a standard Rust idiom that doesn't need unit testing.
}

