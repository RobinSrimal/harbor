//! Test network - simulates a network of Harbor nodes
//!
//! Routes messages between TestNodes without real network connections.

use std::collections::HashMap;

use crate::network::dht::{Id, K};

use super::node::{TestNode, IncomingMessage, OutgoingMessage};

/// Result of a send operation
#[derive(Debug, Clone)]
pub struct SendResult {
    /// Packet ID
    pub packet_id: [u8; 32],
    /// Successfully delivered to
    pub delivered: Vec<usize>,
    /// Failed to deliver to (not subscribed or not in network)
    pub failed: Vec<usize>,
}

/// A simulated network of Harbor nodes
pub struct TestNetwork {
    /// All nodes in the network (index = node id)
    nodes: Vec<TestNode>,
    /// EndpointID -> node index mapping
    endpoint_to_node: HashMap<[u8; 32], usize>,
    /// Topics: TopicId -> member node indices
    topics: HashMap<[u8; 32], Vec<usize>>,
    /// Message delivery delay simulation (for future use)
    _simulated_latency_ms: u64,
    /// Whether to auto-send receipts
    auto_receipts: bool,
}

impl TestNetwork {
    /// Create a new empty test network
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            endpoint_to_node: HashMap::new(),
            topics: HashMap::new(),
            _simulated_latency_ms: 0,
            auto_receipts: true,
        }
    }

    /// Create a network with N nodes
    pub fn with_nodes(count: usize) -> Self {
        let mut network = Self::new();
        for _ in 0..count {
            network.add_node();
        }
        network
    }

    /// Create a network with N deterministic nodes
    pub fn with_deterministic_nodes(count: usize, seed: u8) -> Self {
        let mut network = Self::new();
        for i in 0..count {
            network.add_node_with_seed(seed.wrapping_add(i as u8));
        }
        network
    }

    /// Disable automatic receipt sending (for testing receipt logic)
    pub fn disable_auto_receipts(&mut self) {
        self.auto_receipts = false;
    }

    /// Add a new node to the network
    pub fn add_node(&mut self) -> usize {
        let id = self.nodes.len();
        let node = TestNode::new(id);
        self.endpoint_to_node.insert(node.endpoint_id(), id);
        self.nodes.push(node);
        id
    }

    /// Add a new node with deterministic seed
    pub fn add_node_with_seed(&mut self, seed: u8) -> usize {
        let id = self.nodes.len();
        let node = TestNode::new_with_seed(id, seed);
        self.endpoint_to_node.insert(node.endpoint_id(), id);
        self.nodes.push(node);
        id
    }

    /// Get a node by index
    pub fn node(&self, id: usize) -> &TestNode {
        &self.nodes[id]
    }

    /// Get a mutable node by index
    pub fn node_mut(&mut self, id: usize) -> &mut TestNode {
        &mut self.nodes[id]
    }

    /// Get node by EndpointID
    pub fn node_by_endpoint(&self, endpoint_id: &[u8; 32]) -> Option<&TestNode> {
        self.endpoint_to_node.get(endpoint_id).map(|&id| &self.nodes[id])
    }

    /// Get node index by EndpointID
    pub fn node_index(&self, endpoint_id: &[u8; 32]) -> Option<usize> {
        self.endpoint_to_node.get(endpoint_id).copied()
    }

    /// Get total node count
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Bootstrap the DHT - each node learns about some others
    pub fn bootstrap_dht(&mut self) {
        let endpoints: Vec<[u8; 32]> = self.nodes.iter().map(|n| n.endpoint_id()).collect();
        
        // Each node learns about all others (small network)
        for i in 0..self.nodes.len() {
            for (j, endpoint) in endpoints.iter().enumerate() {
                if i != j {
                    self.nodes[i].add_peer(*endpoint);
                }
            }
        }
    }

    /// Bootstrap DHT with limited knowledge (more realistic)
    pub fn bootstrap_dht_partial(&mut self, bootstrap_count: usize) {
        if self.nodes.is_empty() {
            return;
        }

        let endpoints: Vec<[u8; 32]> = self.nodes.iter().map(|n| n.endpoint_id()).collect();
        
        // First node knows no one initially
        // Each subsequent node learns about up to bootstrap_count previous nodes
        for i in 1..self.nodes.len() {
            let count = bootstrap_count.min(i);
            for j in (i - count)..i {
                self.nodes[i].add_peer(endpoints[j]);
                self.nodes[j].add_peer(endpoints[i]);
            }
        }
    }

    /// Create a topic with the specified members
    pub fn create_topic(&mut self, member_indices: &[usize]) -> [u8; 32] {
        // Generate topic ID from members
        let mut hasher = blake3::Hasher::new();
        for &idx in member_indices {
            hasher.update(&self.nodes[idx].endpoint_id());
        }
        hasher.update(&(self.topics.len() as u64).to_le_bytes());
        let topic_id: [u8; 32] = *hasher.finalize().as_bytes();

        // Get member endpoints
        let member_endpoints: Vec<[u8; 32]> = member_indices
            .iter()
            .map(|&idx| self.nodes[idx].endpoint_id())
            .collect();

        // Subscribe all members
        for &idx in member_indices {
            self.nodes[idx].subscribe(topic_id, member_endpoints.clone());
        }

        // Track topic
        self.topics.insert(topic_id, member_indices.to_vec());

        topic_id
    }

    /// Send a packet from one node to all topic members
    pub fn send_packet(
        &mut self,
        from: usize,
        topic_id: &[u8; 32],
        plaintext: &[u8],
    ) -> Result<SendResult, String> {
        // Get topic members
        let members = self.topics
            .get(topic_id)
            .ok_or("topic not found")?
            .clone();

        // Verify sender is a member of the topic
        if !members.contains(&from) {
            return Err("sender is not a member of the topic".to_string());
        }

        // Create packet
        let packet = self.nodes[from]
            .create_packet(topic_id, plaintext)
            .map_err(|e| e.to_string())?;

        let packet_id = packet.packet_id;
        let sender_endpoint = self.nodes[from].endpoint_id();

        let mut delivered = Vec::new();
        let mut failed = Vec::new();

        // Deliver to all members except sender
        for &member_idx in &members {
            if member_idx == from {
                continue;
            }

            // Clone packet for each recipient
            let packet_clone = packet.clone();

            // Process at recipient
            if let Some(_plaintext) = self.nodes[member_idx].process_packet(sender_endpoint, packet_clone) {
                delivered.push(member_idx);

                // Auto-send receipt if enabled
                if self.auto_receipts {
                    let recipient_endpoint = self.nodes[member_idx].endpoint_id();
                    self.nodes[from].process_receipt(recipient_endpoint, packet_id);
                }
            } else {
                failed.push(member_idx);
            }
        }

        // Record outgoing for sender
        for &member_idx in &delivered {
            let to_endpoint = self.nodes[member_idx].endpoint_id();
            self.nodes[from].outbox.push_back(OutgoingMessage::Packet {
                to: to_endpoint,
                packet: packet.clone(),
            });
        }

        Ok(SendResult {
            packet_id,
            delivered,
            failed,
        })
    }

    /// Manually send a receipt from one node to another
    pub fn send_receipt(
        &mut self,
        from: usize,
        to: usize,
        packet_id: [u8; 32],
    ) {
        let from_endpoint = self.nodes[from].endpoint_id();
        self.nodes[to].process_receipt(from_endpoint, packet_id);
    }

    /// Perform a DHT lookup from one node
    ///
    /// Simulates the iterative lookup algorithm.
    pub fn dht_lookup(&mut self, from: usize, target: Id) -> Vec<[u8; 32]> {
        let mut queried = std::collections::HashSet::new();
        let mut results = std::collections::BTreeSet::new();
        
        // Start with closest known nodes
        let initial = self.nodes[from].find_closest(&target, K);
        let mut candidates: Vec<Id> = initial;

        queried.insert(self.nodes[from].dht_id());

        while !candidates.is_empty() {
            // Get closest unqueried candidate
            candidates.sort_by_key(|id| target.distance(id));
            
            let to_query: Vec<Id> = candidates
                .iter()
                .filter(|id| !queried.contains(id))
                .take(3) // alpha = 3
                .copied()
                .collect();

            if to_query.is_empty() {
                break;
            }

            for candidate_id in to_query {
                queried.insert(candidate_id);

                // Find the node
                if let Some(&node_idx) = self.endpoint_to_node.get(candidate_id.as_bytes()) {
                    // Query this node
                    let response = self.nodes[node_idx].find_closest(&target, K);
                    
                    // Add to results
                    results.insert((target.distance(&candidate_id), candidate_id));

                    // Add new candidates
                    for new_id in response {
                        if !queried.contains(&new_id) {
                            candidates.push(new_id);
                        }
                    }

                    // Update routing table
                    self.nodes[from].add_peer(*candidate_id.as_bytes());
                }
            }

            // Keep only K best results
            while results.len() > K {
                results.pop_last();
            }
        }

        results.into_iter().map(|(_, id)| *id.as_bytes()).collect()
    }

    /// Get all messages in a node's inbox
    pub fn drain_inbox(&mut self, node_id: usize) -> Vec<IncomingMessage> {
        let mut messages = Vec::new();
        while let Some(msg) = self.nodes[node_id].pop_inbox() {
            messages.push(msg);
        }
        messages
    }

    /// Clear all node queues
    pub fn clear_all_queues(&mut self) {
        for node in &mut self.nodes {
            node.clear_queues();
        }
    }

    /// Print network state (for debugging)
    pub fn debug_print(&self) {
        println!("=== TestNetwork ({} nodes) ===", self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            println!(
                "Node {}: endpoint={}, routing_table_size={}, topics={}",
                i,
                hex::encode(&node.endpoint_id()[..4]),
                node.routing_table.len(),
                node.topics.len()
            );
        }
        println!("Topics: {}", self.topics.len());
    }

    // ============ Harbor Simulation ============

    /// Find the K closest nodes to a HarborID (simulates DHT lookup for Harbor Nodes)
    pub fn find_harbor_nodes(&self, harbor_id: &[u8; 32], count: usize) -> Vec<usize> {
        let target = Id::new(*harbor_id);
        
        let mut distances: Vec<(_, usize)> = self.nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| {
                let node_id = Id::new(node.endpoint_id());
                (target.distance(&node_id), idx)
            })
            .collect();

        distances.sort_by(|a, b| a.0.cmp(&b.0));
        distances.into_iter().take(count).map(|(_, idx)| idx).collect()
    }

    /// Simulate replicating a packet to Harbor Nodes when some recipients are offline
    ///
    /// Returns (nodes_stored_on, failed_nodes)
    pub fn replicate_to_harbor(
        &mut self,
        topic_id: &[u8; 32],
        packet_id: [u8; 32],
        packet_data: Vec<u8>,
        undelivered: Vec<usize>,
        replication_factor: usize,
    ) -> (Vec<usize>, Vec<usize>) {
        use crate::security::topic_keys::harbor_id_from_topic;
        
        let harbor_id = harbor_id_from_topic(topic_id);
        let harbor_nodes = self.find_harbor_nodes(&harbor_id, replication_factor * 3);
        
        let undelivered_endpoints: Vec<[u8; 32]> = undelivered
            .iter()
            .map(|&idx| self.nodes[idx].endpoint_id())
            .collect();

        let mut stored_on = Vec::new();
        let mut failed = Vec::new();

        // Store on up to replication_factor nodes
        for &node_idx in &harbor_nodes {
            if stored_on.len() >= replication_factor {
                break;
            }

            // Simulate storing on this node
            // In real implementation this would be a network call
            let node = &mut self.nodes[node_idx];
            
            // Store packet info in node's harbor cache (simulated)
            node.harbor_cache.push((packet_id, harbor_id, packet_data.clone(), undelivered_endpoints.clone()));
            
            stored_on.push(node_idx);
        }

        // Any harbor nodes we couldn't reach are "failed"
        for &node_idx in &harbor_nodes {
            if !stored_on.contains(&node_idx) && stored_on.len() < replication_factor {
                failed.push(node_idx);
            }
        }

        (stored_on, failed)
    }

    /// Simulate pulling packets from Harbor Nodes
    ///
    /// Returns packets that were stored for this recipient (deduplicated)
    pub fn pull_from_harbor(
        &mut self,
        recipient_idx: usize,
        topic_id: &[u8; 32],
    ) -> Vec<Vec<u8>> {
        use crate::security::topic_keys::harbor_id_from_topic;
        use std::collections::HashSet;
        
        let harbor_id = harbor_id_from_topic(topic_id);
        let recipient_endpoint = self.nodes[recipient_idx].endpoint_id();
        
        // Find Harbor Nodes for this topic
        let harbor_nodes = self.find_harbor_nodes(&harbor_id, 30);
        
        // Track which packets we've already pulled (by packet_id)
        let mut seen_packets: HashSet<[u8; 32]> = HashSet::new();
        let mut pulled_packets = Vec::new();

        // Pull from each Harbor Node
        for &node_idx in &harbor_nodes {
            let node = &mut self.nodes[node_idx];
            
            // Find packets for this recipient in this node's harbor cache
            let mut to_update = Vec::new();
            for (i, (packet_id, cached_harbor_id, packet_data, recipients)) in node.harbor_cache.iter().enumerate() {
                if cached_harbor_id == &harbor_id {
                    // Check if this recipient should receive it
                    if recipients.contains(&recipient_endpoint) {
                        // Only add to pulled_packets if we haven't seen this packet before
                        if !seen_packets.contains(packet_id) {
                            pulled_packets.push(packet_data.clone());
                            seen_packets.insert(*packet_id);
                        }
                        to_update.push((i, *packet_id));
                    }
                }
            }

            // Mark as delivered on this node (remove recipient from list)
            for (_idx, packet_id) in to_update.iter() {
                // Find and update the entry
                let mut indices_to_remove = Vec::new();
                for (i, (pid, _, _, recipients)) in node.harbor_cache.iter_mut().enumerate() {
                    if pid == packet_id {
                        recipients.retain(|r| r != &recipient_endpoint);
                        if recipients.is_empty() {
                            indices_to_remove.push(i);
                        }
                    }
                }
                // Remove empty entries (in reverse order to preserve indices)
                for idx in indices_to_remove.into_iter().rev() {
                    node.harbor_cache.remove(idx);
                }
            }
        }

        pulled_packets
    }

    /// Check if a packet is stored on at least N Harbor Nodes
    pub fn is_replicated(&self, topic_id: &[u8; 32], packet_id: [u8; 32], min_nodes: usize) -> bool {
        use crate::security::topic_keys::harbor_id_from_topic;
        
        let harbor_id = harbor_id_from_topic(topic_id);

        let mut count = 0;
        for node in &self.nodes {
            for (pid, hid, _, _) in &node.harbor_cache {
                if pid == &packet_id && hid == &harbor_id {
                    count += 1;
                    break;
                }
            }
        }

        count >= min_nodes
    }
}

impl Default for TestNetwork {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_creation() {
        let network = TestNetwork::new();
        assert_eq!(network.node_count(), 0);
    }

    #[test]
    fn test_add_nodes() {
        let mut network = TestNetwork::new();
        let alice = network.add_node();
        let bob = network.add_node();
        
        assert_eq!(network.node_count(), 2);
        assert_eq!(alice, 0);
        assert_eq!(bob, 1);
    }

    #[test]
    fn test_with_nodes() {
        let network = TestNetwork::with_nodes(5);
        assert_eq!(network.node_count(), 5);
    }

    #[test]
    fn test_deterministic_network() {
        let network1 = TestNetwork::with_deterministic_nodes(3, 42);
        let network2 = TestNetwork::with_deterministic_nodes(3, 42);
        
        for i in 0..3 {
            assert_eq!(
                network1.node(i).endpoint_id(),
                network2.node(i).endpoint_id()
            );
        }
    }

    #[test]
    fn test_bootstrap_dht() {
        let mut network = TestNetwork::with_nodes(5);
        network.bootstrap_dht();
        
        // Each node should know about all others
        for i in 0..5 {
            assert_eq!(network.node(i).routing_table.len(), 4);
        }
    }

    #[test]
    fn test_bootstrap_dht_partial() {
        let mut network = TestNetwork::with_nodes(10);
        network.bootstrap_dht_partial(3);
        
        // First node should know about nodes 1,2,3 that connected to it
        // Last node should know about 3 previous nodes
        assert!(network.node(0).routing_table.len() >= 1);
        assert!(network.node(9).routing_table.len() >= 3);
    }

    #[test]
    fn test_create_topic() {
        let mut network = TestNetwork::with_nodes(3);
        let topic = network.create_topic(&[0, 1, 2]);
        
        // All members should be subscribed
        for i in 0..3 {
            assert!(network.node(i).is_subscribed(&topic));
        }
    }

    #[test]
    fn test_send_packet() {
        let mut network = TestNetwork::with_nodes(3);
        let topic = network.create_topic(&[0, 1, 2]);
        
        let result = network.send_packet(0, &topic, b"Hello everyone!").unwrap();
        
        // Should deliver to nodes 1 and 2
        assert_eq!(result.delivered.len(), 2);
        assert!(result.delivered.contains(&1));
        assert!(result.delivered.contains(&2));
        assert!(result.failed.is_empty());
    }

    #[test]
    fn test_send_packet_receipts() {
        let mut network = TestNetwork::with_nodes(3);
        let topic = network.create_topic(&[0, 1, 2]);
        
        let result = network.send_packet(0, &topic, b"Hello!").unwrap();
        
        // With auto_receipts, sender should have received all receipts
        assert!(network.node(0).all_receipts_received(&result.packet_id));
    }

    #[test]
    fn test_send_packet_manual_receipts() {
        let mut network = TestNetwork::with_nodes(3);
        network.disable_auto_receipts();
        
        let topic = network.create_topic(&[0, 1, 2]);
        let result = network.send_packet(0, &topic, b"Hello!").unwrap();
        
        // Without auto_receipts, sender should NOT have received receipts
        assert!(!network.node(0).all_receipts_received(&result.packet_id));
        
        // Manually send receipts
        network.send_receipt(1, 0, result.packet_id);
        assert!(!network.node(0).all_receipts_received(&result.packet_id));
        
        network.send_receipt(2, 0, result.packet_id);
        assert!(network.node(0).all_receipts_received(&result.packet_id));
    }

    #[test]
    fn test_received_message_content() {
        let mut network = TestNetwork::with_nodes(2);
        let topic = network.create_topic(&[0, 1]);
        
        network.send_packet(0, &topic, b"Secret message").unwrap();
        
        // Check node 1's inbox
        let messages = network.drain_inbox(1);
        assert_eq!(messages.len(), 1);
        
        match &messages[0] {
            IncomingMessage::Packet { plaintext, .. } => {
                assert_eq!(plaintext.as_ref().unwrap(), b"Secret message");
            }
            _ => panic!("expected packet"),
        }
    }

    #[test]
    fn test_non_member_doesnt_receive() {
        let mut network = TestNetwork::with_nodes(3);
        // Only nodes 0 and 1 in topic
        let topic = network.create_topic(&[0, 1]);
        
        network.send_packet(0, &topic, b"Private message").unwrap();
        
        // Node 1 should receive
        assert_eq!(network.drain_inbox(1).len(), 1);
        
        // Node 2 should NOT receive (not in topic)
        assert_eq!(network.drain_inbox(2).len(), 0);
    }

    #[test]
    fn test_dht_lookup() {
        let mut network = TestNetwork::with_deterministic_nodes(10, 100);
        network.bootstrap_dht();
        
        // Node 0 looks for a target
        let target = Id::from_hash(b"some target");
        let results = network.dht_lookup(0, target);
        
        // Should find some nodes
        assert!(!results.is_empty());
        assert!(results.len() <= K);
    }

    #[test]
    fn test_multiple_topics() {
        let mut network = TestNetwork::with_nodes(4);
        
        let topic1 = network.create_topic(&[0, 1]);
        let topic2 = network.create_topic(&[2, 3]);
        
        // Send on topic1
        network.send_packet(0, &topic1, b"Topic 1 message").unwrap();
        
        // Only node 1 receives
        assert_eq!(network.drain_inbox(1).len(), 1);
        assert_eq!(network.drain_inbox(2).len(), 0);
        assert_eq!(network.drain_inbox(3).len(), 0);
        
        // Send on topic2
        network.send_packet(2, &topic2, b"Topic 2 message").unwrap();
        
        // Only node 3 receives
        assert_eq!(network.drain_inbox(1).len(), 0);
        assert_eq!(network.drain_inbox(3).len(), 1);
    }

    #[test]
    fn test_bidirectional_communication() {
        let mut network = TestNetwork::with_nodes(2);
        let topic = network.create_topic(&[0, 1]);
        
        // Node 0 sends to node 1
        network.send_packet(0, &topic, b"Hello from 0").unwrap();
        assert_eq!(network.drain_inbox(1).len(), 1);
        
        // Node 1 sends to node 0
        network.send_packet(1, &topic, b"Hello from 1").unwrap();
        assert_eq!(network.drain_inbox(0).len(), 1);
    }

    // ============ Harbor Tests ============

    #[test]
    fn test_find_harbor_nodes() {
        let network = TestNetwork::with_deterministic_nodes(20, 50);
        
        let topic_id = [42u8; 32];
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);
        
        let harbor_nodes = network.find_harbor_nodes(&harbor_id, 10);
        
        assert_eq!(harbor_nodes.len(), 10);
        
        // Should be sorted by distance
        let target = Id::new(harbor_id);
        for i in 1..harbor_nodes.len() {
            let prev_dist = target.distance(&Id::new(network.node(harbor_nodes[i-1]).endpoint_id()));
            let curr_dist = target.distance(&Id::new(network.node(harbor_nodes[i]).endpoint_id()));
            assert!(prev_dist <= curr_dist);
        }
    }

    #[test]
    fn test_replicate_to_harbor() {
        let mut network = TestNetwork::with_deterministic_nodes(20, 100);
        let topic = network.create_topic(&[0, 1, 2]);
        
        // Send a packet - simulate node 2 being offline (not receiving)
        // We manually create the scenario
        let packet_id = [1u8; 32];
        let packet_data = b"secret message".to_vec();
        
        // Replicate to Harbor Nodes for undelivered recipient (node 2)
        let (stored_on, _failed) = network.replicate_to_harbor(
            &topic,
            packet_id,
            packet_data.clone(),
            vec![2], // node 2 is offline
            5, // replicate to 5 nodes
        );
        
        assert!(stored_on.len() >= 5);
        assert!(network.is_replicated(&topic, packet_id, 5));
    }

    #[test]
    fn test_pull_from_harbor() {
        let mut network = TestNetwork::with_deterministic_nodes(20, 100);
        let topic = network.create_topic(&[0, 1, 2]);
        
        // Replicate a packet for node 2
        let packet_id = [2u8; 32];
        let packet_data = b"missed message".to_vec();
        
        network.replicate_to_harbor(
            &topic,
            packet_id,
            packet_data.clone(),
            vec![2],
            10,
        );
        
        // Node 2 comes online and pulls
        let pulled = network.pull_from_harbor(2, &topic);
        
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0], packet_data);
        
        // Pulling again should return nothing (already delivered)
        let pulled_again = network.pull_from_harbor(2, &topic);
        assert!(pulled_again.is_empty());
    }

    #[test]
    fn test_harbor_multiple_recipients() {
        let mut network = TestNetwork::with_deterministic_nodes(20, 100);
        let topic = network.create_topic(&[0, 1, 2, 3]);
        
        // Nodes 2 and 3 are offline
        let packet_id = [3u8; 32];
        let packet_data = b"group message".to_vec();
        
        network.replicate_to_harbor(
            &topic,
            packet_id,
            packet_data.clone(),
            vec![2, 3],
            10,
        );
        
        // Node 2 pulls
        let pulled_2 = network.pull_from_harbor(2, &topic);
        assert_eq!(pulled_2.len(), 1);
        
        // Node 3 pulls (packet should still be there for them)
        let pulled_3 = network.pull_from_harbor(3, &topic);
        assert_eq!(pulled_3.len(), 1);
        
        // Now both have received, packet should be cleaned up
        // (verified by trying to pull again)
        let pulled_again = network.pull_from_harbor(2, &topic);
        assert!(pulled_again.is_empty());
    }

    #[test]
    fn test_harbor_different_topics() {
        let mut network = TestNetwork::with_deterministic_nodes(20, 100);
        
        let topic1 = network.create_topic(&[0, 1, 2]);
        let topic2 = network.create_topic(&[0, 3, 4]);
        
        // Store for topic1
        network.replicate_to_harbor(&topic1, [1u8; 32], b"topic1 msg".to_vec(), vec![2], 5);
        
        // Store for topic2
        network.replicate_to_harbor(&topic2, [2u8; 32], b"topic2 msg".to_vec(), vec![3, 4], 5);
        
        // Node 2 should only get topic1 message
        let pulled_2 = network.pull_from_harbor(2, &topic1);
        assert_eq!(pulled_2.len(), 1);
        assert_eq!(pulled_2[0], b"topic1 msg".to_vec());
        
        // Node 2 shouldn't get topic2 message (not a member)
        let pulled_2_topic2 = network.pull_from_harbor(2, &topic2);
        assert!(pulled_2_topic2.is_empty());
        
        // Node 3 should get topic2 message
        let pulled_3 = network.pull_from_harbor(3, &topic2);
        assert_eq!(pulled_3.len(), 1);
    }

    #[test]
    fn test_harbor_closest_nodes() {
        // Test that harbor nodes are actually close to the HarborID
        let network = TestNetwork::with_deterministic_nodes(100, 200);
        
        let topic_id = [99u8; 32];
        let harbor_id = crate::security::topic_keys::harbor_id_from_topic(&topic_id);
        let target = Id::new(harbor_id);
        
        // Get closest 30 nodes
        let harbor_nodes = network.find_harbor_nodes(&harbor_id, 30);
        
        // Get all other nodes
        let other_nodes: Vec<usize> = (0..100).filter(|i| !harbor_nodes.contains(i)).collect();
        
        // Verify: the furthest "harbor node" should be closer than the closest "other" node
        let furthest_harbor = harbor_nodes.last().unwrap();
        let furthest_harbor_dist = target.distance(&Id::new(network.node(*furthest_harbor).endpoint_id()));
        
        for other in &other_nodes {
            let other_dist = target.distance(&Id::new(network.node(*other).endpoint_id()));
            assert!(furthest_harbor_dist <= other_dist);
        }
    }

    // ============ Edge Case Tests ============

    #[test]
    fn test_send_to_self() {
        // Test that sending to self works (skips network, auto-delivers)
        let mut network = TestNetwork::with_nodes(1);
        let topic = network.create_topic(&[0]);
        
        // Send to only self
        let result = network.send_packet(0, &topic, b"To myself").unwrap();
        
        // Should succeed but have no external delivery (just self)
        // Self is not counted in delivered list since it's the sender
        assert!(result.failed.is_empty());
    }

    #[test]
    fn test_empty_message() {
        let mut network = TestNetwork::with_nodes(2);
        let topic = network.create_topic(&[0, 1]);
        
        // Empty message should still work
        let result = network.send_packet(0, &topic, b"").unwrap();
        assert_eq!(result.delivered.len(), 1);
        
        let messages = network.drain_inbox(1);
        match &messages[0] {
            IncomingMessage::Packet { plaintext, .. } => {
                assert_eq!(plaintext.as_ref().unwrap(), b"");
            }
            _ => panic!("expected packet"),
        }
    }

    #[test]
    fn test_large_message() {
        let mut network = TestNetwork::with_nodes(2);
        let topic = network.create_topic(&[0, 1]);
        
        // Large message (just under 512KB limit)
        let large_data = vec![0xAB; 500 * 1024];
        let result = network.send_packet(0, &topic, &large_data).unwrap();
        assert_eq!(result.delivered.len(), 1);
        
        let messages = network.drain_inbox(1);
        match &messages[0] {
            IncomingMessage::Packet { plaintext, .. } => {
                assert_eq!(plaintext.as_ref().unwrap().len(), 500 * 1024);
            }
            _ => panic!("expected packet"),
        }
    }

    #[test]
    fn test_sender_not_in_topic() {
        let mut network = TestNetwork::with_nodes(3);
        // Only nodes 1 and 2 in topic
        let topic = network.create_topic(&[1, 2]);
        
        // Node 0 tries to send (not in topic)
        let result = network.send_packet(0, &topic, b"Intruder");
        
        // Should fail because sender is not subscribed
        assert!(result.is_err());
    }

    #[test]
    fn test_many_recipients() {
        let mut network = TestNetwork::with_nodes(20);
        let members: Vec<usize> = (0..20).collect();
        let topic = network.create_topic(&members);
        
        let result = network.send_packet(0, &topic, b"Broadcast").unwrap();
        
        // Should deliver to 19 others
        assert_eq!(result.delivered.len(), 19);
        assert!(result.failed.is_empty());
    }

    #[test]
    fn test_rapid_sends() {
        let mut network = TestNetwork::with_nodes(3);
        let topic = network.create_topic(&[0, 1, 2]);
        
        // Send multiple messages rapidly
        for i in 0..10 {
            let msg = format!("Message {}", i);
            let result = network.send_packet(0, &topic, msg.as_bytes()).unwrap();
            assert_eq!(result.delivered.len(), 2);
        }
        
        // Both recipients should have all 10 messages
        assert_eq!(network.drain_inbox(1).len(), 10);
        assert_eq!(network.drain_inbox(2).len(), 10);
    }

    #[test]
    fn test_receipt_idempotence() {
        let mut network = TestNetwork::with_nodes(2);
        network.disable_auto_receipts();
        
        let topic = network.create_topic(&[0, 1]);
        let result = network.send_packet(0, &topic, b"Hello").unwrap();
        
        // Send receipt multiple times
        network.send_receipt(1, 0, result.packet_id);
        network.send_receipt(1, 0, result.packet_id);
        network.send_receipt(1, 0, result.packet_id);
        
        // Should still be marked as received just once
        assert!(network.node(0).all_receipts_received(&result.packet_id));
    }

    #[test]
    fn test_topic_isolation_keys() {
        // Messages encrypted with one topic's key shouldn't decrypt with another's
        let mut network = TestNetwork::with_nodes(3);
        
        let topic1 = network.create_topic(&[0, 1]);
        let _topic2 = network.create_topic(&[0, 2]);
        
        // Send on topic1
        network.send_packet(0, &topic1, b"Topic 1 secret").unwrap();
        
        // Node 2 shouldn't receive anything (different topic)
        let inbox2 = network.drain_inbox(2);
        assert!(inbox2.is_empty());
        
        // Node 1 should receive the message
        let inbox1 = network.drain_inbox(1);
        assert_eq!(inbox1.len(), 1);
    }

    #[test]
    fn test_node_identity_uniqueness() {
        let network = TestNetwork::with_nodes(100);
        
        let mut endpoint_ids = std::collections::HashSet::new();
        for i in 0..100 {
            let id = network.node(i).endpoint_id();
            assert!(endpoint_ids.insert(id), "Duplicate endpoint ID found");
        }
    }

    #[test]
    fn test_deterministic_network_reproducibility() {
        // Two networks with same seed should have identical nodes
        let network1 = TestNetwork::with_deterministic_nodes(10, 42);
        let network2 = TestNetwork::with_deterministic_nodes(10, 42);
        
        for i in 0..10 {
            assert_eq!(
                network1.node(i).endpoint_id(),
                network2.node(i).endpoint_id()
            );
        }
        
        // Different seed should produce different nodes
        let network3 = TestNetwork::with_deterministic_nodes(10, 43);
        assert_ne!(
            network1.node(0).endpoint_id(),
            network3.node(0).endpoint_id()
        );
    }
}

