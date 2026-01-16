//! Test node - simulates a Harbor node without network
//!
//! Each TestNode has its own identity, routing table, and message queues.

use std::collections::{HashMap, VecDeque};

use crate::network::dht::distance::Id;
use crate::network::dht::routing::RoutingTable;
use crate::security::create_key_pair::{generate_key_pair, KeyPair};
use crate::security::send::packet::SendPacket;
use crate::security::topic_keys::TopicKeys;

/// A simulated Harbor node for testing
#[derive(Debug)]
pub struct TestNode {
    /// Node identifier (index in TestNetwork)
    pub id: usize,
    /// Ed25519 key pair
    pub key_pair: KeyPair,
    /// DHT routing table
    pub routing_table: RoutingTable,
    /// Topics this node is subscribed to: TopicId -> (keys, member EndpointIDs)
    pub topics: HashMap<[u8; 32], TopicMembership>,
    /// Incoming message queue
    pub inbox: VecDeque<IncomingMessage>,
    /// Outgoing message queue (for verification)
    pub outbox: VecDeque<OutgoingMessage>,
    /// Read receipts received
    pub receipts_received: Vec<([u8; 32], [u8; 32])>, // (packet_id, from)
    /// Packets waiting for receipts: packet_id -> pending recipients
    pub pending_receipts: HashMap<[u8; 32], Vec<[u8; 32]>>,
    /// Simulated Harbor cache: (packet_id, harbor_id, packet_data, pending_recipients)
    pub harbor_cache: Vec<([u8; 32], [u8; 32], Vec<u8>, Vec<[u8; 32]>)>,
}

/// Topic membership info
#[derive(Debug, Clone)]
pub struct TopicMembership {
    /// Topic ID
    pub topic_id: [u8; 32],
    /// Derived keys
    pub keys: TopicKeys,
    /// Member EndpointIDs
    pub members: Vec<[u8; 32]>,
}

/// An incoming message
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    /// A Send packet
    Packet {
        from: [u8; 32],
        packet: SendPacket,
        /// Decrypted plaintext (after verification)
        plaintext: Option<Vec<u8>>,
    },
    /// A receipt
    Receipt {
        from: [u8; 32],
        packet_id: [u8; 32],
    },
    /// A DHT FindNode request
    FindNode {
        from: [u8; 32],
        target: Id,
    },
    /// A DHT FindNode response
    FindNodeResponse {
        from: [u8; 32],
        nodes: Vec<[u8; 32]>,
    },
}

/// An outgoing message (for verification)
#[derive(Debug, Clone)]
pub enum OutgoingMessage {
    /// A Send packet
    Packet {
        to: [u8; 32],
        packet: SendPacket,
    },
    /// A receipt
    Receipt {
        to: [u8; 32],
        packet_id: [u8; 32],
    },
    /// A DHT FindNode request
    FindNode {
        to: [u8; 32],
        target: Id,
    },
    /// A DHT FindNode response
    FindNodeResponse {
        to: [u8; 32],
        nodes: Vec<[u8; 32]>,
    },
}

impl TestNode {
    /// Create a new test node
    pub fn new(id: usize) -> Self {
        let key_pair = generate_key_pair();
        let node_id = Id::new(key_pair.public_key);
        
        Self {
            id,
            key_pair,
            routing_table: RoutingTable::new(node_id),
            topics: HashMap::new(),
            inbox: VecDeque::new(),
            outbox: VecDeque::new(),
            receipts_received: Vec::new(),
            pending_receipts: HashMap::new(),
            harbor_cache: Vec::new(),
        }
    }

    /// Create a new test node with a specific seed (for deterministic tests)
    pub fn new_with_seed(id: usize, seed: u8) -> Self {
        // Use seed to create deterministic key
        let mut secret = [0u8; 32];
        secret[0] = seed;
        secret[1] = (id & 0xFF) as u8;
        secret[2] = ((id >> 8) & 0xFF) as u8;
        
        let key_pair = crate::security::create_key_pair::key_pair_from_bytes(&secret);
        let node_id = Id::new(key_pair.public_key);
        
        Self {
            id,
            key_pair,
            routing_table: RoutingTable::new(node_id),
            topics: HashMap::new(),
            inbox: VecDeque::new(),
            outbox: VecDeque::new(),
            receipts_received: Vec::new(),
            pending_receipts: HashMap::new(),
            harbor_cache: Vec::new(),
        }
    }

    /// Get the node's EndpointID (public key)
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.key_pair.public_key
    }

    /// Get the node's DHT ID
    pub fn dht_id(&self) -> Id {
        Id::new(self.key_pair.public_key)
    }

    /// Subscribe to a topic
    pub fn subscribe(&mut self, topic_id: [u8; 32], members: Vec<[u8; 32]>) {
        let keys = TopicKeys::derive(&topic_id);
        self.topics.insert(topic_id, TopicMembership {
            topic_id,
            keys,
            members,
        });
    }

    /// Unsubscribe from a topic
    pub fn unsubscribe(&mut self, topic_id: &[u8; 32]) {
        self.topics.remove(topic_id);
    }

    /// Check if subscribed to a topic
    pub fn is_subscribed(&self, topic_id: &[u8; 32]) -> bool {
        self.topics.contains_key(topic_id)
    }

    /// Get topic members (if subscribed)
    pub fn get_topic_members(&self, topic_id: &[u8; 32]) -> Option<&[[u8; 32]]> {
        self.topics.get(topic_id).map(|m| m.members.as_slice())
    }

    /// Add a peer to the routing table
    pub fn add_peer(&mut self, peer_id: [u8; 32]) -> bool {
        self.routing_table.add_node(Id::new(peer_id))
    }

    /// Remove a peer from the routing table
    pub fn remove_peer(&mut self, peer_id: &[u8; 32]) {
        self.routing_table.remove_node(&Id::new(*peer_id));
    }

    /// Find closest nodes in routing table
    pub fn find_closest(&self, target: &Id, k: usize) -> Vec<Id> {
        self.routing_table.find_closest_nodes(target, k)
    }

    /// Process an incoming packet
    ///
    /// Verifies and decrypts the packet if we have the topic keys.
    /// Returns the decrypted plaintext if successful.
    pub fn process_packet(&mut self, from: [u8; 32], packet: SendPacket) -> Option<Vec<u8>> {
        // Find the topic by harbor_id
        let topic = self.topics.values().find(|t| {
            crate::security::topic_keys::harbor_id_from_topic(&t.topic_id) == packet.harbor_id
        })?;

        // Verify and decrypt
        let plaintext = crate::security::verify_and_decrypt_packet(&packet, &topic.topic_id).ok()?;

        // Store in inbox
        self.inbox.push_back(IncomingMessage::Packet {
            from,
            packet,
            plaintext: Some(plaintext.clone()),
        });

        Some(plaintext)
    }

    /// Create and queue a packet for sending
    pub fn create_packet(
        &mut self,
        topic_id: &[u8; 32],
        plaintext: &[u8],
    ) -> Result<SendPacket, crate::security::PacketError> {
        let packet = crate::security::create_packet(
            topic_id,
            &self.key_pair.private_key,
            &self.key_pair.public_key,
            plaintext,
        )?;

        // Track pending receipts
        if let Some(membership) = self.topics.get(topic_id) {
            let recipients: Vec<[u8; 32]> = membership.members
                .iter()
                .filter(|m| **m != self.endpoint_id())
                .copied()
                .collect();
            self.pending_receipts.insert(packet.packet_id, recipients);
        }

        Ok(packet)
    }

    /// Process an incoming receipt
    pub fn process_receipt(&mut self, from: [u8; 32], packet_id: [u8; 32]) {
        self.receipts_received.push((packet_id, from));
        
        // Remove from pending
        if let Some(pending) = self.pending_receipts.get_mut(&packet_id) {
            pending.retain(|p| *p != from);
        }
    }

    /// Check if all receipts received for a packet
    pub fn all_receipts_received(&self, packet_id: &[u8; 32]) -> bool {
        self.pending_receipts
            .get(packet_id)
            .map(|p| p.is_empty())
            .unwrap_or(true)
    }

    /// Get next message from inbox
    pub fn pop_inbox(&mut self) -> Option<IncomingMessage> {
        self.inbox.pop_front()
    }

    /// Peek at inbox without removing
    pub fn peek_inbox(&self) -> Option<&IncomingMessage> {
        self.inbox.front()
    }

    /// Clear all queues
    pub fn clear_queues(&mut self) {
        self.inbox.clear();
        self.outbox.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_creation() {
        let node = TestNode::new(0);
        
        assert_eq!(node.id, 0);
        assert_eq!(node.endpoint_id().len(), 32);
        assert!(node.topics.is_empty());
        assert!(node.inbox.is_empty());
    }

    #[test]
    fn test_node_deterministic_creation() {
        let node1 = TestNode::new_with_seed(0, 42);
        let node2 = TestNode::new_with_seed(0, 42);
        
        // Same seed = same keys
        assert_eq!(node1.endpoint_id(), node2.endpoint_id());
        
        // Different seed = different keys
        let node3 = TestNode::new_with_seed(0, 43);
        assert_ne!(node1.endpoint_id(), node3.endpoint_id());
    }

    #[test]
    fn test_subscribe_unsubscribe() {
        let mut node = TestNode::new(0);
        let topic_id = [42u8; 32];
        
        assert!(!node.is_subscribed(&topic_id));
        
        node.subscribe(topic_id, vec![[1u8; 32], [2u8; 32]]);
        assert!(node.is_subscribed(&topic_id));
        
        node.unsubscribe(&topic_id);
        assert!(!node.is_subscribed(&topic_id));
    }

    #[test]
    fn test_add_peer_to_routing() {
        let mut node = TestNode::new(0);
        let peer = [1u8; 32];
        
        assert!(node.add_peer(peer));
        assert!(node.routing_table.contains(&Id::new(peer)));
    }

    #[test]
    fn test_create_and_process_packet() {
        let mut alice = TestNode::new(0);
        let mut bob = TestNode::new(1);
        
        let topic_id = [42u8; 32];
        let members = vec![alice.endpoint_id(), bob.endpoint_id()];
        
        // Both subscribe
        alice.subscribe(topic_id, members.clone());
        bob.subscribe(topic_id, members);
        
        // Alice creates packet
        let packet = alice.create_packet(&topic_id, b"Hello Bob!").unwrap();
        
        // Bob processes it
        let plaintext = bob.process_packet(alice.endpoint_id(), packet);
        
        assert_eq!(plaintext, Some(b"Hello Bob!".to_vec()));
    }

    #[test]
    fn test_receipt_tracking() {
        let mut alice = TestNode::new(0);
        let bob_id = [1u8; 32];
        let carol_id = [2u8; 32];
        
        let topic_id = [42u8; 32];
        alice.subscribe(topic_id, vec![alice.endpoint_id(), bob_id, carol_id]);
        
        // Create packet
        let packet = alice.create_packet(&topic_id, b"Hello!").unwrap();
        let packet_id = packet.packet_id;
        
        // Not all receipts yet
        assert!(!alice.all_receipts_received(&packet_id));
        
        // Bob sends receipt
        alice.process_receipt(bob_id, packet_id);
        assert!(!alice.all_receipts_received(&packet_id));
        
        // Carol sends receipt
        alice.process_receipt(carol_id, packet_id);
        assert!(alice.all_receipts_received(&packet_id));
    }
}

