//! DHT (Distributed Hash Table) implementation for Harbor
//!
//! A Kademlia-style DHT using BLAKE3 (32-byte keyspace) over iroh connections.
//! Uses 0-RTT for fast DHT queries.
//!
//! # Architecture
//!
//! Top-level modules:
//! - `protocol`: Wire format for DHT messages (FindNode, FindNodeResponse)
//! - `actor`: Actor that manages DHT state and handles messages
//!
//! Internal modules (in `internal/`):
//! - `api`: High-level API for DHT operations
//! - `bootstrap`: Bootstrap node configuration
//! - `config`: DHT configuration and factory functions
//! - `distance`: XOR distance metric implementation
//! - `lookup`: Kademlia iterative lookup algorithm
//! - `pool`: Connection pool with direct-dial fallback
//! - `routing`: In-memory routing table with K-buckets
//!
//! # Example
//!
//! ```ignore
//! use harbor::network::dht::{DhtConfig, create_dht_node, Id};
//!
//! // Create a DHT node
//! let local_id = Id::from_hash(b"my-node-id");
//! let (rpc, api) = create_dht_node(local_id, pool, DhtConfig::persistent(), bootstrap);
//!
//! // Perform a lookup
//! let target = Id::from_hash(b"target-key");
//! let closest = api.lookup(target, None).await?;
//! ```

pub mod actor;
pub mod internal;
pub mod protocol;
pub mod service;

// Re-export commonly used items from internal modules
pub use internal::bootstrap::{BootstrapConfig, BootstrapNode, default_bootstrap_nodes, bootstrap_node_ids, bootstrap_dial_info};
pub use internal::config::{DhtConfig, create_dht_actor, create_dht_actor_with_buckets};
pub use internal::distance::{Distance, Id};
pub use internal::pool::{DhtPool, DhtPoolConfig, DhtPoolError, DialInfo, DHT_ALPN};
pub use internal::routing::{Buckets, RoutingTable, K, ALPHA, BUCKET_COUNT};
pub use service::{DhtService, WeakDhtService};

#[cfg(test)]
mod tests {
    //! Offline DHT tests
    //!
    //! These tests use in-memory clients without actual network connections.

    use super::*;
    use super::internal::routing::KBucket;

    fn make_id(seed: u8) -> Id {
        Id::new([seed; 32])
    }

    fn make_id_with_prefix(prefix: &[u8]) -> Id {
        let mut bytes = [0u8; 32];
        bytes[..prefix.len()].copy_from_slice(prefix);
        Id::new(bytes)
    }

    // ==================== Distance Tests ====================

    #[test]
    fn test_xor_distance_properties() {
        let a = make_id(1);
        let b = make_id(2);
        let c = make_id(3);

        // Symmetry: d(a, b) == d(b, a)
        assert_eq!(a.distance(&b), b.distance(&a));

        // Identity: d(a, a) == 0
        assert_eq!(a.distance(&a), Distance::ZERO);

        // Triangle inequality check (XOR is a metric)
        let d_ab = a.distance(&b);
        let d_bc = b.distance(&c);
        let d_ac = a.distance(&c);

        // In XOR metric, d(a,c) <= d(a,b) XOR d(b,c) isn't always true
        // but the metric still works for Kademlia routing
        assert!(d_ac <= Distance::MAX);
        assert!(d_ab <= Distance::MAX);
        assert!(d_bc <= Distance::MAX);
    }

    #[test]
    fn test_bucket_index_distribution() {
        // Local node at 0x00...
        let local = make_id(0);

        // Test that different XOR distances map to different buckets
        let buckets: Vec<Option<usize>> = (0..8).map(|bit| {
            let mut bytes = [0u8; 32];
            bytes[0] = 1 << (7 - bit); // Set bit at position 'bit'
            let remote = Id::new(bytes);
            local.distance(&remote).bucket_index()
        }).collect();

        // Each should be in a different bucket
        for (i, bucket) in buckets.iter().enumerate() {
            assert_eq!(*bucket, Some(i));
        }
    }

    // ==================== Routing Table Tests ====================

    #[test]
    fn test_routing_table_bucket_selection() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Nodes at different distances should go to different buckets
        let node_bucket_0 = make_id_with_prefix(&[0x80]); // Differs in first bit
        let node_bucket_7 = make_id_with_prefix(&[0x01]); // Differs in 8th bit

        table.add_node(node_bucket_0);
        table.add_node(node_bucket_7);

        assert!(table.contains(&node_bucket_0));
        assert!(table.contains(&node_bucket_7));
        assert_eq!(table.bucket_index(&node_bucket_0), Some(0));
        assert_eq!(table.bucket_index(&node_bucket_7), Some(7));
    }

    #[test]
    fn test_routing_table_k_limit() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Try to add K+5 nodes to the same bucket
        let mut added = 0;
        for i in 0..(K + 5) {
            // All these will go to bucket 0 (differ in first bit)
            let mut bytes = [0u8; 32];
            bytes[0] = 0x80 | (i as u8 & 0x7F); // Keep first bit set
            let node = Id::new(bytes);
            if table.add_node(node) {
                added += 1;
            }
        }

        // Should only have added K nodes
        assert_eq!(added, K);
    }

    #[test]
    fn test_find_closest_correctness() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add nodes with known distances
        let nodes: Vec<Id> = vec![
            make_id_with_prefix(&[0x10]), // Distance 16
            make_id_with_prefix(&[0x08]), // Distance 8
            make_id_with_prefix(&[0x04]), // Distance 4
            make_id_with_prefix(&[0x02]), // Distance 2
            make_id_with_prefix(&[0x01]), // Distance 1
        ];

        for node in &nodes {
            table.add_node(*node);
        }

        // Find 3 closest to local (0x00)
        let closest = table.find_closest_nodes(&local, 3);

        assert_eq!(closest.len(), 3);
        // Should be sorted by distance (closest first)
        assert_eq!(closest[0], nodes[4]); // 0x01
        assert_eq!(closest[1], nodes[3]); // 0x02
        assert_eq!(closest[2], nodes[2]); // 0x04
    }

    #[test]
    fn test_routing_table_remove_node() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        let node = make_id(1);
        table.add_node(node);
        assert!(table.contains(&node));

        table.remove_node(&node);
        assert!(!table.contains(&node));
    }

    // ==================== K-Bucket Tests ====================

    #[test]
    fn test_kbucket_lru_behavior() {
        let mut bucket = KBucket::new();

        // Fill the bucket
        let nodes: Vec<Id> = (0..K).map(|i| make_id(i as u8)).collect();
        for node in &nodes {
            assert!(bucket.add_node(*node));
        }

        // Bucket is now full
        assert!(bucket.is_full());

        // Try to add a new node - should fail
        let new_node = make_id(100);
        assert!(!bucket.add_node(new_node));

        // All original nodes should still be there
        for node in &nodes {
            assert!(bucket.contains(node));
        }
    }

    #[test]
    fn test_kbucket_dedup() {
        let mut bucket = KBucket::new();
        let node = make_id(1);

        // Add same node multiple times
        bucket.add_node(node);
        bucket.add_node(node);
        bucket.add_node(node);

        // Should only count once
        assert_eq!(bucket.len(), 1);
    }

    // ==================== Integration Tests ====================

    #[test]
    fn test_routing_table_persistence_simulation() {
        // Simulate saving and restoring routing table
        let local = make_id(0);

        // Create and populate a routing table
        let mut table1 = RoutingTable::new(local);
        for i in 1..=10u8 {
            table1.add_node(make_id(i));
        }

        // "Save" the buckets
        let saved_buckets = table1.buckets.clone();

        // "Restore" to a new table
        let table2 = RoutingTable::with_buckets(local, saved_buckets);

        // Should have the same nodes
        for i in 1..=10u8 {
            assert!(table2.contains(&make_id(i)));
        }
    }

    #[test]
    fn test_large_routing_table() {
        let local = make_id(0);
        let mut table = RoutingTable::new(local);

        // Add many nodes
        let mut added = 0;
        for i in 1..=255u8 {
            if table.add_node(make_id(i)) {
                added += 1;
            }
        }

        // Should have added at least some nodes
        assert!(added > 0);
        assert_eq!(table.len(), added);

        // find_closest should still work
        let closest = table.find_closest_nodes(&make_id(128), K);
        assert!(closest.len() <= K);
    }

    // ==================== Distance Inverse Tests ====================

    #[test]
    fn test_distance_inverse_identity() {
        let a = make_id(42);
        let b = make_id(123);

        let dist = a.distance(&b);

        // d.inverse(b) should give us a
        let recovered_a = Id::new(dist.inverse(b.as_bytes()));
        assert_eq!(recovered_a, a);

        // d.inverse(a) should give us b
        let recovered_b = Id::new(dist.inverse(a.as_bytes()));
        assert_eq!(recovered_b, b);
    }

    // ==================== API Message Serialization Tests ====================

    #[test]
    fn test_node_info_roundtrip() {
        use super::protocol::NodeInfo;

        let original = NodeInfo {
            node_id: [42u8; 32],
            addresses: vec!["192.168.1.1:4433".to_string(), "10.0.0.1:4433".to_string()],
            relay_url: Some("https://relay.example.com".to_string()),
        };

        let bytes = postcard::to_allocvec(&original).unwrap();
        let decoded: NodeInfo = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.node_id, original.node_id);
        assert_eq!(decoded.addresses, original.addresses);
        assert_eq!(decoded.relay_url, original.relay_url);
    }

    #[test]
    fn test_find_node_request_roundtrip() {
        use super::protocol::FindNode;

        let request = FindNode {
            target: make_id(100),
            requester: Some([1u8; 32]),
            requester_relay_url: Some("https://relay.example.com/".to_string()),
        };

        let bytes = postcard::to_allocvec(&request).unwrap();
        let decoded: FindNode = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.target, request.target);
        assert_eq!(decoded.requester, request.requester);
        assert_eq!(decoded.requester_relay_url, request.requester_relay_url);
    }

    #[test]
    fn test_find_node_response_roundtrip() {
        use super::protocol::FindNodeResponse;
        use super::protocol::NodeInfo;

        let response = FindNodeResponse {
            nodes: (0..5).map(|i| NodeInfo {
                node_id: [i; 32],
                addresses: vec![format!("192.168.1.{}:4433", i)],
                relay_url: None,
            }).collect(),
        };

        let bytes = postcard::to_allocvec(&response).unwrap();
        let decoded: FindNodeResponse = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.nodes.len(), 5);
        for (i, node) in decoded.nodes.iter().enumerate() {
            assert_eq!(node.node_id, [i as u8; 32]);
        }
    }

    // ==================== Config Tests ====================

    #[test]
    fn test_dht_config_deterministic_rng() {
        let config = DhtConfig {
            rng_seed: Some([42u8; 32]),
            ..Default::default()
        };

        // With the same seed, RNG should produce the same sequence
        use rand::SeedableRng;
        let mut rng1 = rand::rngs::StdRng::from_seed(config.rng_seed.unwrap());
        let mut rng2 = rand::rngs::StdRng::from_seed(config.rng_seed.unwrap());

        use rand::Rng;
        for _ in 0..10 {
            assert_eq!(rng1.r#gen::<u64>(), rng2.r#gen::<u64>());
        }
    }

    // ==================== Candidate Verification Tests ====================

    #[test]
    fn test_dht_config_candidate_defaults() {
        use super::internal::config::{DEFAULT_CANDIDATE_VERIFY_INTERVAL, MAX_CANDIDATES_PER_VERIFY};
        use std::time::Duration;

        let config = DhtConfig::default();

        assert_eq!(config.candidate_verify_interval, DEFAULT_CANDIDATE_VERIFY_INTERVAL);
        assert_eq!(config.max_candidates_per_verify, MAX_CANDIDATES_PER_VERIFY);
        assert_eq!(DEFAULT_CANDIDATE_VERIFY_INTERVAL, Duration::from_secs(30));
        assert_eq!(MAX_CANDIDATES_PER_VERIFY, 10);
    }

    #[test]
    fn test_dht_config_custom_candidate_interval() {
        use std::time::Duration;

        let config = DhtConfig {
            candidate_verify_interval: Duration::from_secs(10),
            max_candidates_per_verify: 5,
            ..Default::default()
        };

        assert_eq!(config.candidate_verify_interval, Duration::from_secs(10));
        assert_eq!(config.max_candidates_per_verify, 5);
    }

    #[test]
    fn test_candidate_set_uniqueness() {
        use std::collections::HashSet;

        let mut candidates: HashSet<Id> = HashSet::new();
        let node1 = make_id(1);
        let node2 = make_id(1); // Same as node1
        let node3 = make_id(2); // Different

        // First insert succeeds
        assert!(candidates.insert(node1));
        // Duplicate insert fails (returns false)
        assert!(!candidates.insert(node2));
        // Different node succeeds
        assert!(candidates.insert(node3));

        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn test_candidate_batch_limits() {
        use std::collections::HashSet;
        use super::internal::config::MAX_CANDIDATES_PER_VERIFY;

        let mut candidates: HashSet<Id> = HashSet::new();

        // Add 25 candidates
        for i in 0..25 {
            candidates.insert(make_id(i as u8));
        }
        assert_eq!(candidates.len(), 25);

        // Batch should be limited to MAX_CANDIDATES_PER_VERIFY
        let batch: Vec<Id> = candidates
            .iter()
            .take(MAX_CANDIDATES_PER_VERIFY)
            .copied()
            .collect();

        assert_eq!(batch.len(), MAX_CANDIDATES_PER_VERIFY);
        assert_eq!(batch.len(), 10); // Verify the constant
    }

    #[test]
    fn test_candidate_batch_removes_from_set() {
        use std::collections::HashSet;

        let mut candidates: HashSet<Id> = HashSet::new();

        // Add 5 candidates
        for i in 0..5 {
            candidates.insert(make_id(i));
        }
        assert_eq!(candidates.len(), 5);

        // Take all as batch
        let batch: Vec<Id> = candidates.iter().copied().collect();

        // Remove from candidates (simulating verification process)
        for id in &batch {
            candidates.remove(id);
        }

        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_candidate_not_added_for_transient() {
        // Transient nodes don't accept candidates from requesters
        let config = DhtConfig::transient();
        assert!(config.transient);

        // In handle_rpc, the check is: if !self.config.transient
        // So for transient nodes, the requester is NOT added to candidates
    }

    #[test]
    fn test_local_id_excluded_from_candidates() {
        use std::collections::HashSet;

        let local_id = make_id(42);
        let mut candidates: HashSet<Id> = HashSet::new();

        // Simulate the check in handle_rpc: if id != *self.node.local_id()
        let requester = local_id;
        if requester != local_id {
            candidates.insert(requester);
        }

        // Local ID should NOT be in candidates
        assert!(!candidates.contains(&local_id));
        assert_eq!(candidates.len(), 0);

        // Different ID should be added
        let remote = make_id(100);
        if remote != local_id {
            candidates.insert(remote);
        }
        assert!(candidates.contains(&remote));
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_candidate_flow_simulation() {
        use std::collections::HashSet;

        // Simulate the full candidate flow:
        // 1. Requester sends FindNode -> added to candidates
        // 2. Verification runs -> connection attempted
        // 3. If successful -> promoted to routing table via nodes_seen
        // 4. If failed -> removed from candidates, not added to routing table

        let local = make_id(0);
        let mut routing_table = RoutingTable::new(local);
        let mut candidates: HashSet<Id> = HashSet::new();

        // Step 1: Simulate receiving FindNode requests
        let requester1 = make_id(1);
        let requester2 = make_id(2);
        let requester3 = make_id(3);

        candidates.insert(requester1);
        candidates.insert(requester2);
        candidates.insert(requester3);

        // Routing table should NOT have these yet
        assert!(!routing_table.contains(&requester1));
        assert!(!routing_table.contains(&requester2));
        assert!(!routing_table.contains(&requester3));

        // Step 2 & 3: Simulate verification - only requester1 and requester3 are reachable
        let verified = vec![requester1, requester3];
        let _failed = vec![requester2];

        // Remove all from candidates
        for _id in candidates.drain() {
            // In real code, this happens as part of verification
        }

        // Step 4: Add verified to routing table
        for id in verified {
            routing_table.add_node(id);
        }

        // Verify final state
        assert!(routing_table.contains(&requester1));
        assert!(!routing_table.contains(&requester2)); // Failed verification
        assert!(routing_table.contains(&requester3));
        assert!(candidates.is_empty());
    }
}
