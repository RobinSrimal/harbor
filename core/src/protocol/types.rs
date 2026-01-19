//! Types for the high-level API

use serde::{Deserialize, Serialize};

// ============================================================================
// Protocol Events (for application event bus)
// ============================================================================

/// Events emitted by the protocol for the application layer
/// 
/// Applications can subscribe to these events to update their UI
/// (e.g., show new messages, file progress bars, notifications)
#[derive(Debug, Clone)]
pub enum ProtocolEvent {
    /// A text message was received in a topic
    Message(IncomingMessage),
    /// A file was shared to a topic (announcement received)
    FileAnnounced(FileAnnouncedEvent),
    /// File download progress update
    FileProgress(FileProgressEvent),
    /// File download completed
    FileComplete(FileCompleteEvent),
}

/// An incoming message from the event bus
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// The topic this message belongs to
    pub topic_id: [u8; 32],
    /// The sender's EndpointID
    pub sender_id: [u8; 32],
    /// The message payload (app defines format)
    pub payload: Vec<u8>,
    /// Timestamp when the message was created (Unix seconds)
    pub timestamp: i64,
}

/// Event: A file was shared to a topic
/// 
/// Emitted when a FileAnnouncement is received from another member
#[derive(Debug, Clone)]
pub struct FileAnnouncedEvent {
    /// The topic this file was shared to
    pub topic_id: [u8; 32],
    /// Who shared the file
    pub source_id: [u8; 32],
    /// BLAKE3 hash of the file (unique identifier)
    pub hash: [u8; 32],
    /// Human-readable filename
    pub display_name: String,
    /// Total file size in bytes
    pub total_size: u64,
    /// Total number of chunks
    pub total_chunks: u32,
    /// Timestamp when announced
    pub timestamp: i64,
}

/// Event: File download progress update
/// 
/// Emitted periodically as chunks are received
#[derive(Debug, Clone)]
pub struct FileProgressEvent {
    /// File hash
    pub hash: [u8; 32],
    /// Chunks downloaded so far
    pub chunks_complete: u32,
    /// Total chunks in the file
    pub total_chunks: u32,
}

/// Event: File download completed
/// 
/// Emitted when all chunks have been received
#[derive(Debug, Clone)]
pub struct FileCompleteEvent {
    /// File hash
    pub hash: [u8; 32],
    /// Filename
    pub display_name: String,
    /// Total size in bytes
    pub total_size: u64,
}

/// Member info for topic invites
/// 
/// Contains the EndpointID and optional relay URL for connectivity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberInfo {
    /// The member's EndpointID (32-byte public key)
    pub endpoint_id: [u8; 32],
    /// Optional relay URL for cross-network connectivity
    pub relay_url: Option<String>,
}

impl MemberInfo {
    /// Create member info with just an endpoint ID (no relay)
    pub fn new(endpoint_id: [u8; 32]) -> Self {
        Self { endpoint_id, relay_url: None }
    }

    /// Create member info with endpoint ID and relay URL
    pub fn with_relay(endpoint_id: [u8; 32], relay_url: String) -> Self {
        Self { endpoint_id, relay_url: Some(relay_url) }
    }
}

/// An invite to join a topic
///
/// This contains all the information needed for another peer to join,
/// including relay URLs for cross-network connectivity.
/// Can be serialized and shared (e.g., via QR code, link, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicInvite {
    /// The topic identifier
    pub topic_id: [u8; 32],
    /// Current members with their connection info
    pub member_info: Vec<MemberInfo>,
    /// Legacy: EndpointIDs only (for backwards compatibility)
    #[serde(default)]
    pub members: Vec<[u8; 32]>,
}

impl TopicInvite {
    /// Create a new topic invite with member info (includes relay URLs)
    pub fn new_with_info(topic_id: [u8; 32], member_info: Vec<MemberInfo>) -> Self {
        let members = member_info.iter().map(|m| m.endpoint_id).collect();
        Self { topic_id, member_info, members }
    }

    /// Create a new topic invite (legacy, no relay info)
    pub fn new(topic_id: [u8; 32], members: Vec<[u8; 32]>) -> Self {
        let member_info = members.iter().map(|id| MemberInfo::new(*id)).collect();
        Self { topic_id, member_info, members }
    }

    /// Get all member endpoint IDs
    pub fn member_ids(&self) -> Vec<[u8; 32]> {
        if !self.member_info.is_empty() {
            self.member_info.iter().map(|m| m.endpoint_id).collect()
        } else {
            self.members.iter().copied().collect()
        }
    }

    /// Get member info (with relay URLs if available)
    pub fn get_member_info(&self) -> &[MemberInfo] {
        &self.member_info
    }

    /// Serialize to bytes (for sharing)
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        postcard::to_allocvec(self)
            .map_err(|e| ProtocolError::InvalidInvite(format!("serialization failed: {}", e)))
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        postcard::from_bytes(bytes).map_err(|e| ProtocolError::InvalidInvite(e.to_string()))
    }

    /// Serialize to hex (for text sharing)
    pub fn to_hex(&self) -> Result<String, ProtocolError> {
        Ok(hex::encode(self.to_bytes()?))
    }

    /// Deserialize from hex
    pub fn from_hex(s: &str) -> Result<Self, ProtocolError> {
        let bytes = hex::decode(s)
            .map_err(|e| ProtocolError::InvalidInvite(e.to_string()))?;
        Self::from_bytes(&bytes)
    }
}

/// Errors that can occur in the protocol
#[derive(Debug)]
pub enum ProtocolError {
    /// Failed to start the protocol
    StartFailed(String),
    /// Database error
    Database(String),
    /// Network error
    Network(String),
    /// Not a member of the topic
    NotMember,
    /// Topic not found
    TopicNotFound,
    /// Invalid invite data
    InvalidInvite(String),
    /// Protocol is not running
    NotRunning,
    /// Message too large (max 512KB)
    MessageTooLarge,
    /// Invalid input provided
    InvalidInput(String),
    /// Resource not found
    NotFound(String),
    /// IO error
    Io(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::StartFailed(e) => write!(f, "failed to start protocol: {}", e),
            ProtocolError::Database(e) => write!(f, "database error: {}", e),
            ProtocolError::Network(e) => write!(f, "network error: {}", e),
            ProtocolError::NotMember => write!(f, "not a member of this topic"),
            ProtocolError::TopicNotFound => write!(f, "topic not found"),
            ProtocolError::InvalidInvite(e) => write!(f, "invalid invite: {}", e),
            ProtocolError::NotRunning => write!(f, "protocol is not running"),
            ProtocolError::MessageTooLarge => write!(f, "message too large (max 512KB)"),
            ProtocolError::InvalidInput(e) => write!(f, "invalid input: {}", e),
            ProtocolError::NotFound(e) => write!(f, "not found: {}", e),
            ProtocolError::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        ProtocolError::Io(e.to_string())
    }
}

// ============================================================================
// Stats Types for Dashboard
// ============================================================================

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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn test_topic_invite_new() {
        let topic_id = test_id(1);
        let members = vec![test_id(10), test_id(11)];
        let invite = TopicInvite::new(topic_id, members.clone());

        assert_eq!(invite.topic_id, topic_id);
        assert_eq!(invite.members, members);
    }

    #[test]
    fn test_topic_invite_serialization() {
        let invite = TopicInvite::new(
            test_id(1),
            vec![test_id(10), test_id(11), test_id(12)],
        );

        // Test bytes serialization
        let bytes = invite.to_bytes().unwrap();
        let restored = TopicInvite::from_bytes(&bytes).unwrap();
        assert_eq!(restored.topic_id, invite.topic_id);
        assert_eq!(restored.members.len(), 3);
    }

    #[test]
    fn test_topic_invite_empty_members() {
        let invite = TopicInvite::new(test_id(1), vec![]);

        let bytes = invite.to_bytes().unwrap();
        let restored = TopicInvite::from_bytes(&bytes).unwrap();
        assert_eq!(restored.topic_id, invite.topic_id);
        assert!(restored.members.is_empty());
    }

    #[test]
    fn test_topic_invite_many_members() {
        let members: Vec<[u8; 32]> = (0..100).map(|i| test_id(i)).collect();
        let invite = TopicInvite::new(test_id(255), members.clone());

        let bytes = invite.to_bytes().unwrap();
        let restored = TopicInvite::from_bytes(&bytes).unwrap();
        assert_eq!(restored.members.len(), 100);
    }

    #[test]
    fn test_topic_invite_hex() {
        let invite = TopicInvite::new(
            test_id(1),
            vec![test_id(10), test_id(11)],
        );

        // Test hex serialization
        let hex_str = invite.to_hex().unwrap();
        let restored = TopicInvite::from_hex(&hex_str).unwrap();
        assert_eq!(restored.topic_id, invite.topic_id);
        assert_eq!(restored.members.len(), 2);
    }

    #[test]
    fn test_topic_invite_hex_roundtrip() {
        let invite = TopicInvite::new(test_id(42), vec![test_id(1), test_id(2), test_id(3)]);
        
        let hex1 = invite.to_hex().unwrap();
        let restored1 = TopicInvite::from_hex(&hex1).unwrap();
        let hex2 = restored1.to_hex().unwrap();
        
        // Should be identical after roundtrip
        assert_eq!(hex1, hex2);
    }

    #[test]
    fn test_invalid_invite_bytes() {
        let result = TopicInvite::from_bytes(b"garbage");
        assert!(result.is_err());

        let result = TopicInvite::from_bytes(&[]);
        assert!(result.is_err());

        let result = TopicInvite::from_bytes(&[0; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_invite_hex() {
        let result = TopicInvite::from_hex("not-valid-hex!!!");
        assert!(result.is_err());

        let result = TopicInvite::from_hex("zzzz");
        assert!(result.is_err());

        let result = TopicInvite::from_hex("");
        assert!(result.is_err());
    }

    #[test]
    fn test_incoming_message_structure() {
        let msg = IncomingMessage {
            topic_id: test_id(1),
            sender_id: test_id(2),
            payload: b"Hello world".to_vec(),
            timestamp: 1234567890,
        };

        assert_eq!(msg.topic_id, test_id(1));
        assert_eq!(msg.sender_id, test_id(2));
        assert_eq!(msg.payload, b"Hello world");
        assert_eq!(msg.timestamp, 1234567890);
    }

    #[test]
    fn test_protocol_error_display() {
        let err = ProtocolError::NotMember;
        assert_eq!(err.to_string(), "not a member of this topic");

        let err = ProtocolError::MessageTooLarge;
        assert_eq!(err.to_string(), "message too large (max 512KB)");

        let err = ProtocolError::TopicNotFound;
        assert_eq!(err.to_string(), "topic not found");

        let err = ProtocolError::NotRunning;
        assert_eq!(err.to_string(), "protocol is not running");

        let err = ProtocolError::Database("test error".to_string());
        assert_eq!(err.to_string(), "database error: test error");

        let err = ProtocolError::Network("connection failed".to_string());
        assert_eq!(err.to_string(), "network error: connection failed");

        let err = ProtocolError::InvalidInvite("bad format".to_string());
        assert_eq!(err.to_string(), "invalid invite: bad format");

        let err = ProtocolError::StartFailed("no endpoint".to_string());
        assert_eq!(err.to_string(), "failed to start protocol: no endpoint");
    }

    #[test]
    fn test_protocol_error_is_error_trait() {
        let err: Box<dyn std::error::Error> = Box::new(ProtocolError::NotMember);
        assert!(!err.to_string().is_empty());
    }
}

