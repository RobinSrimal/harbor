//! DHT high-level API
//!
//! Provides operations that affect the entire DHT network:
//! - Lookup: Find the K closest nodes to a target
//! - NodesSeen: Update routing table when nodes are verified
//! - NodesDead: Remove unresponsive nodes from routing table

use irpc::channel::{oneshot, none::NoSender};
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

use super::distance::Id;
use crate::network::dht::protocol::NodeInfo;

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
    /// Returns (node_id, relay_url, verified_at) tuples
    #[rpc(tx = oneshot::Sender<Vec<([u8; 32], String, Option<i64>)>>)]
    #[wrap(GetNodeRelayUrls)]
    GetNodeRelayUrls,

    /// Set node relay URLs (for restoration from database)
    #[rpc(tx = NoSender)]
    #[wrap(SetNodeRelayUrls)]
    SetNodeRelayUrls {
        /// List of (node_id, relay_url, verified_at) tuples
        relay_urls: Vec<([u8; 32], String, Option<i64>)>,
    },

    /// Confirm relay URLs after successful outbound connections
    #[rpc(tx = NoSender)]
    #[wrap(ConfirmRelayUrls)]
    ConfirmRelayUrls {
        /// (node_id, relay_url, verified_at) — verified_at is Some if relay confirmed, None if DNS fallback
        confirmed: Vec<([u8; 32], String, Option<i64>)>,
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

}

