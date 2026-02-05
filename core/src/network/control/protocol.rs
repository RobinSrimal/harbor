//! Control protocol wire format
//!
//! Uses irpc for typed RPC over QUIC bi-streams.
//! One-off exchanges: open connection, send message, get ack, close.
//!
//! Message types:
//! - Connect Request/Accept/Decline: Peer-level connection handshake
//! - Topic Invite: Send topic key + metadata to a peer
//! - Topic Join/Leave: Announce membership changes to topic members
//! - Remove Member: Key rotation + new epoch key distribution
//! - Suggest: Introduce a third party peer

use irpc::channel::oneshot;
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

use crate::resilience::ProofOfWork;

/// Control protocol ALPN
pub const CONTROL_ALPN: &[u8] = b"harbor/control/0";

/// Control packet type prefixes (0x80-0x87)
///
/// These match the PacketType byte values for control messages.
/// Verification mode:
/// - TopicJoin uses MacOnly (sender may be unknown to some recipients)
/// - All others use Full verification (MAC + signature)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlPacketType {
    ConnectRequest = 0x80,
    ConnectAccept = 0x81,
    ConnectDecline = 0x82,
    TopicInvite = 0x83,
    TopicJoin = 0x84,
    TopicLeave = 0x85,
    RemoveMember = 0x86,
    Suggest = 0x87,
}

impl ControlPacketType {
    /// Returns true if this packet type uses MacOnly verification
    /// (for packets where sender may be unknown to recipient)
    pub fn is_mac_only(&self) -> bool {
        matches!(self, ControlPacketType::TopicJoin)
    }

    /// Convert from byte
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x80 => Some(Self::ConnectRequest),
            0x81 => Some(Self::ConnectAccept),
            0x82 => Some(Self::ConnectDecline),
            0x83 => Some(Self::TopicInvite),
            0x84 => Some(Self::TopicJoin),
            0x85 => Some(Self::TopicLeave),
            0x86 => Some(Self::RemoveMember),
            0x87 => Some(Self::Suggest),
            _ => None,
        }
    }
}

// =============================================================================
// Acknowledgment response
// =============================================================================

/// Simple acknowledgment for control messages
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlAck {
    /// The message being acknowledged (request_id or message_id)
    pub message_id: [u8; 32],
    /// Whether the operation succeeded
    pub success: bool,
    /// Optional reason (for declines/failures)
    pub reason: Option<String>,
}

impl ControlAck {
    pub fn success(message_id: [u8; 32]) -> Self {
        Self {
            message_id,
            success: true,
            reason: None,
        }
    }

    pub fn failure(message_id: [u8; 32], reason: impl Into<String>) -> Self {
        Self {
            message_id,
            success: false,
            reason: Some(reason.into()),
        }
    }
}

// =============================================================================
// Connection messages
// =============================================================================

/// Request a peer-level connection
///
/// Harbor ID for replication: `recipient_id`
/// Encryption: DM shared key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectRequest {
    /// Unique identifier for this request (for tracking)
    pub request_id: [u8; 32],
    /// Sender's endpoint ID (must match QUIC peer ID)
    pub sender_id: [u8; 32],
    /// Optional display name
    pub display_name: Option<String>,
    /// Sender's relay URL for NAT traversal
    pub relay_url: Option<String>,
    /// Optional one-time token (for QR code / invite string flow)
    pub token: Option<[u8; 32]>,
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

/// Accept a connection request
///
/// Harbor ID for replication: `requester_id`
/// Encryption: DM shared key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectAccept {
    /// The request_id being accepted
    pub request_id: [u8; 32],
    /// Acceptor's endpoint ID (must match QUIC peer ID)
    pub sender_id: [u8; 32],
    /// Optional display name
    pub display_name: Option<String>,
    /// Acceptor's relay URL for NAT traversal
    pub relay_url: Option<String>,
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

/// Decline a connection request
///
/// Harbor ID for replication: `requester_id`
/// Encryption: DM shared key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectDecline {
    /// The request_id being declined
    pub request_id: [u8; 32],
    /// Decliner's endpoint ID
    pub sender_id: [u8; 32],
    /// Optional reason for declining
    pub reason: Option<String>,
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

// =============================================================================
// Topic messages
// =============================================================================

/// Invite a peer to a topic
///
/// Harbor ID for replication: `recipient_id`
/// Encryption: DM shared key (topic key is the payload)
///
/// Contains all information needed to join the topic:
/// - `topic_id`: The topic secret (used for key derivation)
/// - `epoch` + `epoch_key`: Current epoch key (always explicit, even for epoch 0)
/// - `admin_id`: The topic administrator who controls membership
/// - `members`: Current member list (for routing TopicJoin and populating local DB)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicInvite {
    /// Unique message ID
    pub message_id: [u8; 32],
    /// Sender's endpoint ID
    pub sender_id: [u8; 32],
    /// The topic being invited to (32-byte secret)
    pub topic_id: [u8; 32],
    /// Human-readable topic name
    pub topic_name: Option<String>,
    /// Current epoch number
    pub epoch: u64,
    /// Current epoch key (always explicit - cannot be derived for epoch > 0)
    pub epoch_key: [u8; 32],
    /// Topic administrator's endpoint ID
    pub admin_id: [u8; 32],
    /// Current member list (endpoint IDs)
    pub members: Vec<[u8; 32]>,
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

/// Join a topic (after accepting an invite)
///
/// Sent to all topic members to announce membership.
/// Harbor ID for replication: `hash(topic_id)`
/// Encryption: Topic epoch key
/// Verification: MacOnly (sender may be unknown to some recipients)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicJoin {
    /// Unique message ID
    pub message_id: [u8; 32],
    /// Harbor ID for this topic (hash of topic_id)
    pub harbor_id: [u8; 32],
    /// Joiner's endpoint ID
    pub sender_id: [u8; 32],
    /// Epoch the join is valid for
    pub epoch: u64,
    /// Joiner's relay URL
    pub relay_url: Option<String>,
    /// Membership proof (BLAKE3(topic_id || harbor_id || sender_id))
    pub membership_proof: [u8; 32],
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

/// Leave a topic
///
/// Sent to all topic members to announce departure.
/// Harbor ID for replication: `hash(topic_id)`
/// Encryption: Topic epoch key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicLeave {
    /// Unique message ID
    pub message_id: [u8; 32],
    /// Harbor ID for this topic (hash of topic_id)
    pub harbor_id: [u8; 32],
    /// Leaver's endpoint ID
    pub sender_id: [u8; 32],
    /// Current epoch
    pub epoch: u64,
    /// Membership proof (BLAKE3(topic_id || harbor_id || sender_id))
    pub membership_proof: [u8; 32],
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

/// Remove a member from a topic (admin only)
///
/// Delivers the new epoch key to remaining members.
/// Harbor ID for replication: `recipient_id` (sent individually to each remaining member)
/// Encryption: DM shared key (cannot use topic key since we're rotating it)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveMember {
    /// Unique message ID
    pub message_id: [u8; 32],
    /// Harbor ID for this topic (hash of topic_id)
    pub harbor_id: [u8; 32],
    /// Admin's endpoint ID
    pub sender_id: [u8; 32],
    /// Removed member's endpoint ID
    pub removed_member: [u8; 32],
    /// New epoch number (N+1)
    pub new_epoch: u64,
    /// New epoch key (32 bytes)
    pub new_epoch_key: [u8; 32],
    /// Membership proof (BLAKE3(topic_id || harbor_id || sender_id))
    pub membership_proof: [u8; 32],
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

// =============================================================================
// Introduction messages
// =============================================================================

/// Suggest a peer introduction
///
/// A introduces C to B. B can then decide to connect to C.
/// Harbor ID for replication: `recipient_id`
/// Encryption: DM shared key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggest {
    /// Unique message ID
    pub message_id: [u8; 32],
    /// Introducer's endpoint ID
    pub sender_id: [u8; 32],
    /// Suggested peer's endpoint ID
    pub suggested_peer: [u8; 32],
    /// Suggested peer's relay URL
    pub relay_url: Option<String>,
    /// Optional note about the suggested peer
    pub note: Option<String>,
    /// Proof of Work (context: sender_id || message_type_byte)
    pub pow: ProofOfWork,
}

// =============================================================================
// RPC Protocol
// =============================================================================

/// Control RPC protocol definition using irpc
#[rpc_requests(message = ControlRpcMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum ControlRpcProtocol {
    /// Request peer connection
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    ConnectRequest(ConnectRequest),

    /// Accept connection request
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    ConnectAccept(ConnectAccept),

    /// Decline connection request
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    ConnectDecline(ConnectDecline),

    /// Invite peer to topic
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    TopicInvite(TopicInvite),

    /// Announce topic join
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    TopicJoin(TopicJoin),

    /// Announce topic leave
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    TopicLeave(TopicLeave),

    /// Remove member (key rotation)
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    RemoveMember(RemoveMember),

    /// Suggest peer introduction
    #[rpc(tx = oneshot::Sender<ControlAck>)]
    Suggest(Suggest),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a dummy PoW for tests (difficulty 0 = always passes)
    fn test_pow() -> ProofOfWork {
        ProofOfWork {
            timestamp: 0,
            nonce: 0,
            difficulty_bits: 0,
        }
    }

    #[test]
    fn test_control_packet_type_byte_conversion() {
        assert_eq!(
            ControlPacketType::from_byte(0x80),
            Some(ControlPacketType::ConnectRequest)
        );
        assert_eq!(
            ControlPacketType::from_byte(0x84),
            Some(ControlPacketType::TopicJoin)
        );
        assert_eq!(ControlPacketType::from_byte(0xFF), None);
    }

    #[test]
    fn test_mac_only_verification() {
        assert!(!ControlPacketType::ConnectRequest.is_mac_only());
        assert!(!ControlPacketType::TopicInvite.is_mac_only());
        assert!(ControlPacketType::TopicJoin.is_mac_only());
        assert!(!ControlPacketType::TopicLeave.is_mac_only());
    }

    #[test]
    fn test_control_ack_serialization() {
        let ack = ControlAck::success([1u8; 32]);
        let bytes = postcard::to_allocvec(&ack).unwrap();
        let decoded: ControlAck = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, ack);

        let ack_fail = ControlAck::failure([2u8; 32], "not authorized");
        let bytes = postcard::to_allocvec(&ack_fail).unwrap();
        let decoded: ControlAck = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.success, false);
        assert_eq!(decoded.reason, Some("not authorized".to_string()));
    }

    #[test]
    fn test_connect_request_serialization() {
        let req = ConnectRequest {
            request_id: [1u8; 32],
            sender_id: [2u8; 32],
            display_name: Some("Alice".to_string()),
            relay_url: Some("https://relay.example.com".to_string()),
            token: None,
            pow: test_pow(),
        };
        let bytes = postcard::to_allocvec(&req).unwrap();
        let decoded: ConnectRequest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.request_id, req.request_id);
        assert_eq!(decoded.sender_id, req.sender_id);
        assert_eq!(decoded.display_name, req.display_name);
    }

    #[test]
    fn test_connect_request_with_token() {
        let req = ConnectRequest {
            request_id: [1u8; 32],
            sender_id: [2u8; 32],
            display_name: None,
            relay_url: None,
            token: Some([42u8; 32]),
            pow: test_pow(),
        };
        let bytes = postcard::to_allocvec(&req).unwrap();
        let decoded: ConnectRequest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.token, Some([42u8; 32]));
    }

    #[test]
    fn test_topic_invite_serialization() {
        let invite = TopicInvite {
            message_id: [1u8; 32],
            sender_id: [2u8; 32],
            topic_id: [3u8; 32],
            topic_name: Some("Test Topic".to_string()),
            epoch: 1,
            epoch_key: [4u8; 32],
            admin_id: [5u8; 32],
            members: vec![[2u8; 32], [6u8; 32]],
            pow: test_pow(),
        };
        let bytes = postcard::to_allocvec(&invite).unwrap();
        let decoded: TopicInvite = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.topic_id, invite.topic_id);
        assert_eq!(decoded.epoch, 1);
        assert_eq!(decoded.admin_id, invite.admin_id);
        assert_eq!(decoded.members.len(), 2);
    }

    #[test]
    fn test_topic_join_serialization() {
        use crate::network::membership::create_membership_proof;
        use crate::security::harbor_id_from_topic;

        let join = TopicJoin {
            message_id: [1u8; 32],
            harbor_id: harbor_id_from_topic(&[2u8; 32]),
            sender_id: [3u8; 32],
            epoch: 5,
            relay_url: Some("https://relay.test".to_string()),
            membership_proof: create_membership_proof(
                &[2u8; 32],
                &harbor_id_from_topic(&[2u8; 32]),
                &[3u8; 32],
            ),
            pow: test_pow(),
        };
        let bytes = postcard::to_allocvec(&join).unwrap();
        let decoded: TopicJoin = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.harbor_id, join.harbor_id);
        assert_eq!(decoded.epoch, 5);
    }

    #[test]
    fn test_remove_member_serialization() {
        use crate::network::membership::create_membership_proof;
        use crate::security::harbor_id_from_topic;

        let topic_id = [2u8; 32];
        let harbor_id = harbor_id_from_topic(&topic_id);
        let admin_id = [3u8; 32];

        let remove = RemoveMember {
            message_id: [1u8; 32],
            harbor_id,
            sender_id: admin_id,
            removed_member: [4u8; 32],
            new_epoch: 6,
            new_epoch_key: [5u8; 32],
            membership_proof: create_membership_proof(&topic_id, &harbor_id, &admin_id),
            pow: test_pow(),
        };
        let bytes = postcard::to_allocvec(&remove).unwrap();
        let decoded: RemoveMember = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.removed_member, remove.removed_member);
        assert_eq!(decoded.new_epoch, 6);
        assert_eq!(decoded.new_epoch_key, remove.new_epoch_key);
    }

    #[test]
    fn test_suggest_serialization() {
        let suggest = Suggest {
            message_id: [1u8; 32],
            sender_id: [2u8; 32],
            suggested_peer: [3u8; 32],
            relay_url: Some("https://peer.relay".to_string()),
            note: Some("Great developer!".to_string()),
            pow: test_pow(),
        };
        let bytes = postcard::to_allocvec(&suggest).unwrap();
        let decoded: Suggest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.suggested_peer, suggest.suggested_peer);
        assert_eq!(decoded.note, suggest.note);
    }
}
