//! Stats types for the Protocol dashboard
//!
//! These types are used for monitoring and displaying protocol state.

use serde::{Deserialize, Serialize};

/// Overall protocol statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolStats {
    /// This node's identity
    pub identity: IdentityStats,
    /// Network status
    pub network: NetworkStats,
    /// DHT statistics
    pub dht: DhtStats,
    /// Topic statistics
    pub topics: TopicsStats,
    /// Outgoing packet statistics
    pub outgoing: OutgoingStats,
    /// Harbor node statistics (if acting as Harbor)
    pub harbor: HarborStats,
}

/// Identity information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityStats {
    /// This node's EndpointID (hex-encoded)
    pub endpoint_id: String,
    /// Current relay URL (if connected)
    pub relay_url: Option<String>,
}

/// Network connection status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStats {
    /// Whether the node is online (connected to relay)
    pub is_online: bool,
    /// Number of active connections
    pub active_connections: u32,
}

/// DHT statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtStats {
    /// Total nodes in routing table (from database - persisted)
    pub total_nodes: u32,
    /// Total nodes in in-memory routing table (real-time, more accurate)
    pub in_memory_nodes: u32,
    /// Number of non-empty buckets
    pub active_buckets: u32,
    /// Bootstrap node status
    pub bootstrap_connected: bool,
}

/// DHT bucket information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtBucketInfo {
    /// Bucket index (0-255)
    pub bucket_index: u8,
    /// Number of nodes in this bucket
    pub node_count: u32,
    /// Nodes in this bucket (endpoint IDs as hex)
    pub nodes: Vec<DhtNodeInfo>,
}

/// Information about a DHT node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DhtNodeInfo {
    /// Node's EndpointID (hex-encoded)
    pub endpoint_id: String,
    /// Last known address
    pub address: Option<String>,
    /// Relay URL if known
    pub relay_url: Option<String>,
    /// Whether address is fresh (< 24 hours old)
    pub is_fresh: bool,
}

/// Topics overview statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicsStats {
    /// Number of subscribed topics
    pub subscribed_count: u32,
    /// Total members across all topics
    pub total_members: u32,
}

/// Detailed information about a topic
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicDetails {
    /// Topic ID (hex-encoded)
    pub topic_id: String,
    /// Harbor ID for this topic (hex-encoded)
    pub harbor_id: String,
    /// Members of this topic
    pub members: Vec<TopicMemberInfo>,
    /// Number of Harbor nodes known for this topic
    pub harbor_node_count: u32,
}

/// Information about a topic member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicMemberInfo {
    /// Member's EndpointID (hex-encoded)
    pub endpoint_id: String,
    /// Relay URL if known
    pub relay_url: Option<String>,
    /// Whether this is the local node
    pub is_self: bool,
}

/// Outgoing packet statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingStats {
    /// Total packets pending delivery
    pub pending_count: u32,
    /// Packets awaiting receipts
    pub awaiting_receipts: u32,
    /// Packets replicated to Harbor
    pub replicated_to_harbor: u32,
}

/// Harbor node statistics (when acting as Harbor)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarborStats {
    /// Number of packets stored
    pub packets_stored: u32,
    /// Total storage used (bytes)
    pub storage_bytes: u64,
    /// Number of unique HarborIDs being served
    pub harbor_ids_served: u32,
}

/// Summary of all topics (for listing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicSummary {
    /// Topic ID (hex-encoded)
    pub topic_id: String,
    /// Number of members
    pub member_count: u32,
}
