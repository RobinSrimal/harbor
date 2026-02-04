//! Harbor protocol wire format
//!
//! Defines message types for Harbor Node communication.

use serde::{Deserialize, Serialize};

/// ALPN for Harbor protocol
pub const HARBOR_ALPN: &[u8] = b"harbor/store/0";

/// Harbor protocol message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HarborMessage {
    /// Request to store a packet
    Store(StoreRequest),
    /// Response to store request
    StoreResponse(StoreResponse),
    /// Request packets for a recipient
    Pull(PullRequest),
    /// Response with packets
    PullResponse(PullResponse),
    /// Acknowledge packet delivery
    Ack(DeliveryAck),
    /// Sync request between Harbor Nodes
    SyncRequest(SyncRequest),
    /// Sync response
    SyncResponse(SyncResponse),
}

impl HarborMessage {
    /// Encode message to bytes
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("serialization should not fail")
    }

    /// Decode message from bytes
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        postcard::from_bytes(bytes).map_err(|e| DecodeError::InvalidMessage(e.to_string()))
    }
}

/// Packet type for Harbor storage
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarborPacketType {
    /// Regular content packet - requires full verification
    Content,
    /// Join control packet - MAC-only verification (sender key unknown)
    Join,
    /// Leave control packet - full verification
    Leave,
}

impl HarborPacketType {
    /// Check if this packet type requires signature verification
    pub fn requires_signature(&self) -> bool {
        match self {
            HarborPacketType::Content => true,
            HarborPacketType::Join => false, // Sender's key not known yet
            HarborPacketType::Leave => true,
        }
    }
}

/// Request to store a packet on a Harbor Node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreRequest {
    /// The serialized SendPacket
    pub packet_data: Vec<u8>,
    /// Packet ID (for deduplication)
    pub packet_id: [u8; 32],
    /// HarborID (for routing/validation)
    pub harbor_id: [u8; 32],
    /// Sender's EndpointID
    pub sender_id: [u8; 32],
    /// Recipients who haven't received the packet
    pub recipients: Vec<[u8; 32]>,
    /// Type of packet (affects verification mode)
    pub packet_type: HarborPacketType,
}

/// Response to store request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreResponse {
    /// Packet ID being acknowledged
    pub packet_id: [u8; 32],
    /// Whether the store was successful
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
}

/// Request packets for a specific recipient
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    /// HarborID to pull packets for
    pub harbor_id: [u8; 32],
    /// Requesting client's EndpointID
    pub recipient_id: [u8; 32],
    /// Only return packets created after this timestamp
    /// (used for new members who shouldn't get old packets)
    pub since_timestamp: i64,
    /// Packet IDs the client already has (to avoid re-sending)
    pub already_have: Vec<[u8; 32]>,
    /// Optional relay URL for member discovery
    /// Used by Harbor Nodes to track member connectivity info
    #[serde(default)]
    pub relay_url: Option<String>,
}

/// Response with packets for recipient
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResponse {
    /// HarborID these packets belong to
    pub harbor_id: [u8; 32],
    /// Packets for this recipient
    pub packets: Vec<PacketInfo>,
}

/// Info about a packet in pull response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacketInfo {
    /// Packet ID
    pub packet_id: [u8; 32],
    /// Sender's EndpointID
    pub sender_id: [u8; 32],
    /// Serialized packet data
    pub packet_data: Vec<u8>,
    /// When the packet was created
    pub created_at: i64,
    /// Type of packet (for verification mode)
    pub packet_type: HarborPacketType,
}

/// Acknowledge that a packet was received
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAck {
    /// Packet ID being acknowledged
    pub packet_id: [u8; 32],
    /// Recipient who received it
    pub recipient_id: [u8; 32],
}

/// Sync request between Harbor Nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    /// HarborID to sync
    pub harbor_id: [u8; 32],
    /// Packet IDs we already have
    pub have_packets: Vec<[u8; 32]>,
    /// Delivery status updates (packet_id -> delivered recipients)
    pub delivery_updates: Vec<DeliveryUpdate>,
    /// Member entries we have (for sync)
    #[serde(default)]
    pub member_entries: Vec<MemberSyncEntry>,
}

/// Member sync entry - compact info for determining what needs syncing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberSyncEntry {
    /// The member's endpoint ID
    pub endpoint_id: [u8; 32],
    /// When this member was last seen
    pub last_seen: i64,
    /// Whether this member is evicted
    pub is_evicted: bool,
}

/// Delivery status update for sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryUpdate {
    pub packet_id: [u8; 32],
    pub delivered_to: Vec<[u8; 32]>,
}

/// Sync response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    /// HarborID synced
    pub harbor_id: [u8; 32],
    /// Packets the requester is missing
    pub missing_packets: Vec<PacketInfo>,
    /// Their delivery updates for us
    pub delivery_updates: Vec<DeliveryUpdate>,
    /// Members the requester is missing or has outdated
    #[serde(default)]
    pub members: Vec<MemberFullInfo>,
}

/// Full member info for sync responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberFullInfo {
    /// The member's endpoint ID
    pub endpoint_id: [u8; 32],
    /// Optional relay URL
    pub relay_url: Option<String>,
    /// When this member was last seen
    pub last_seen: i64,
    /// Whether this member is evicted
    pub is_evicted: bool,
    /// Eviction signature (if evicted)
    pub eviction_signature: Option<Vec<u8>>,
}

/// Decode error
#[derive(Debug, Clone)]
pub enum DecodeError {
    InvalidMessage(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::InvalidMessage(e) => write!(f, "invalid message: {}", e),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_request_roundtrip() {
        let msg = HarborMessage::Store(StoreRequest {
            packet_data: b"encrypted packet data".to_vec(),
            packet_id: [1u8; 32],
            harbor_id: [2u8; 32],
            sender_id: [3u8; 32],
            recipients: vec![[10u8; 32], [11u8; 32]],
            packet_type: HarborPacketType::Content,
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::Store(req) = decoded {
            assert_eq!(req.packet_id, [1u8; 32]);
            assert_eq!(req.harbor_id, [2u8; 32]);
            assert_eq!(req.recipients.len(), 2);
            assert_eq!(req.packet_type, HarborPacketType::Content);
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_store_response_roundtrip() {
        let msg = HarborMessage::StoreResponse(StoreResponse {
            packet_id: [1u8; 32],
            success: true,
            error: None,
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::StoreResponse(resp) = decoded {
            assert!(resp.success);
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_pull_request_roundtrip() {
        let msg = HarborMessage::Pull(PullRequest {
            harbor_id: [1u8; 32],
            recipient_id: [2u8; 32],
            since_timestamp: 1704067200,
            already_have: vec![[10u8; 32]],
            relay_url: None,
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::Pull(req) = decoded {
            assert_eq!(req.harbor_id, [1u8; 32]);
            assert_eq!(req.since_timestamp, 1704067200);
            assert_eq!(req.relay_url, None);
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_pull_request_with_relay_url() {
        let msg = HarborMessage::Pull(PullRequest {
            harbor_id: [1u8; 32],
            recipient_id: [2u8; 32],
            since_timestamp: 1704067200,
            already_have: vec![],
            relay_url: Some("https://relay.example.com/".to_string()),
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::Pull(req) = decoded {
            assert_eq!(req.relay_url, Some("https://relay.example.com/".to_string()));
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_pull_response_roundtrip() {
        let msg = HarborMessage::PullResponse(PullResponse {
            harbor_id: [1u8; 32],
            packets: vec![
                PacketInfo {
                    packet_id: [10u8; 32],
                    sender_id: [20u8; 32],
                    packet_data: b"packet 1".to_vec(),
                    created_at: 1704067200,
                    packet_type: HarborPacketType::Content,
                },
                PacketInfo {
                    packet_id: [11u8; 32],
                    sender_id: [21u8; 32],
                    packet_data: b"packet 2".to_vec(),
                    created_at: 1704067201,
                    packet_type: HarborPacketType::Join,
                },
            ],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::PullResponse(resp) = decoded {
            assert_eq!(resp.packets.len(), 2);
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_delivery_ack_roundtrip() {
        let msg = HarborMessage::Ack(DeliveryAck {
            packet_id: [1u8; 32],
            recipient_id: [2u8; 32],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::Ack(ack) = decoded {
            assert_eq!(ack.packet_id, [1u8; 32]);
            assert_eq!(ack.recipient_id, [2u8; 32]);
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_sync_request_roundtrip() {
        let msg = HarborMessage::SyncRequest(SyncRequest {
            harbor_id: [1u8; 32],
            have_packets: vec![[10u8; 32], [11u8; 32]],
            delivery_updates: vec![DeliveryUpdate {
                packet_id: [10u8; 32],
                delivered_to: vec![[100u8; 32]],
            }],
            member_entries: vec![],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::SyncRequest(req) = decoded {
            assert_eq!(req.have_packets.len(), 2);
            assert_eq!(req.delivery_updates.len(), 1);
            assert!(req.member_entries.is_empty());
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_sync_request_with_members() {
        let msg = HarborMessage::SyncRequest(SyncRequest {
            harbor_id: [1u8; 32],
            have_packets: vec![],
            delivery_updates: vec![],
            member_entries: vec![
                MemberSyncEntry {
                    endpoint_id: [10u8; 32],
                    last_seen: 12345,
                    is_evicted: false,
                },
                MemberSyncEntry {
                    endpoint_id: [11u8; 32],
                    last_seen: 12300,
                    is_evicted: true,
                },
            ],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::SyncRequest(req) = decoded {
            assert_eq!(req.member_entries.len(), 2);
            assert_eq!(req.member_entries[0].endpoint_id, [10u8; 32]);
            assert!(!req.member_entries[0].is_evicted);
            assert!(req.member_entries[1].is_evicted);
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_decode_invalid() {
        let result = HarborMessage::decode(b"invalid data");
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_pull_response() {
        let msg = HarborMessage::PullResponse(PullResponse {
            harbor_id: [1u8; 32],
            packets: vec![],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::PullResponse(resp) = decoded {
            assert!(resp.packets.is_empty());
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_sync_response_roundtrip() {
        let msg = HarborMessage::SyncResponse(SyncResponse {
            harbor_id: [1u8; 32],
            missing_packets: vec![PacketInfo {
                packet_id: [10u8; 32],
                sender_id: [20u8; 32],
                packet_data: b"packet data".to_vec(),
                created_at: 1704067200,
                packet_type: HarborPacketType::Content,
            }],
            delivery_updates: vec![DeliveryUpdate {
                packet_id: [11u8; 32],
                delivered_to: vec![[100u8; 32], [101u8; 32]],
            }],
            members: vec![],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::SyncResponse(resp) = decoded {
            assert_eq!(resp.harbor_id, [1u8; 32]);
            assert_eq!(resp.missing_packets.len(), 1);
            assert_eq!(resp.delivery_updates.len(), 1);
            assert_eq!(resp.delivery_updates[0].delivered_to.len(), 2);
            assert!(resp.members.is_empty());
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_sync_response_with_members() {
        let msg = HarborMessage::SyncResponse(SyncResponse {
            harbor_id: [1u8; 32],
            missing_packets: vec![],
            delivery_updates: vec![],
            members: vec![
                MemberFullInfo {
                    endpoint_id: [10u8; 32],
                    relay_url: Some("https://relay.com/".to_string()),
                    last_seen: 12345,
                    is_evicted: false,
                    eviction_signature: None,
                },
                MemberFullInfo {
                    endpoint_id: [11u8; 32],
                    relay_url: None,
                    last_seen: 12300,
                    is_evicted: true,
                    eviction_signature: Some(vec![1, 2, 3]),
                },
            ],
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::SyncResponse(resp) = decoded {
            assert_eq!(resp.members.len(), 2);
            assert_eq!(resp.members[0].endpoint_id, [10u8; 32]);
            assert!(!resp.members[0].is_evicted);
            assert!(resp.members[1].is_evicted);
            assert_eq!(resp.members[1].eviction_signature, Some(vec![1, 2, 3]));
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_harbor_packet_type_requires_signature() {
        // Content requires signature (sender should be known)
        assert!(HarborPacketType::Content.requires_signature());
        
        // Join does NOT require signature (sender's key not known yet)
        assert!(!HarborPacketType::Join.requires_signature());
        
        // Leave requires signature (sender should be known member)
        assert!(HarborPacketType::Leave.requires_signature());
    }

    #[test]
    fn test_decode_error_display() {
        let err = DecodeError::InvalidMessage("test error".to_string());
        assert_eq!(err.to_string(), "invalid message: test error");
    }

    #[test]
    fn test_store_response_with_error() {
        let msg = HarborMessage::StoreResponse(StoreResponse {
            packet_id: [1u8; 32],
            success: false,
            error: Some("packet too large".to_string()),
        });

        let encoded = msg.encode();
        let decoded = HarborMessage::decode(&encoded).unwrap();

        if let HarborMessage::StoreResponse(resp) = decoded {
            assert!(!resp.success);
            assert_eq!(resp.error, Some("packet too large".to_string()));
        } else {
            panic!("wrong message type");
        }
    }

    #[test]
    fn test_packet_info_all_types() {
        // Test each packet type in PacketInfo
        for packet_type in [HarborPacketType::Content, HarborPacketType::Join, HarborPacketType::Leave] {
            let info = PacketInfo {
                packet_id: [1u8; 32],
                sender_id: [2u8; 32],
                packet_data: b"data".to_vec(),
                created_at: 12345,
                packet_type,
            };
            
            let msg = HarborMessage::PullResponse(PullResponse {
                harbor_id: [0u8; 32],
                packets: vec![info],
            });
            
            let encoded = msg.encode();
            let decoded = HarborMessage::decode(&encoded).unwrap();
            
            if let HarborMessage::PullResponse(resp) = decoded {
                assert_eq!(resp.packets[0].packet_type, packet_type);
            }
        }
    }

}

