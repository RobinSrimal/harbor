//! Send protocol wire format
//!
//! Uses irpc for typed RPC over QUIC bi-streams:
//! - DeliverTopic: Send raw topic payload, receive a Receipt as response
//! - DeliverDm: Send raw DM payload, receive a Receipt as response
//!
//! QUIC TLS provides transport encryption - no app-level crypto needed for direct delivery.
//! For Harbor storage, packets are sealed on-the-fly during replication (see seal module).

use irpc::channel::oneshot;
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

pub use crate::network::membership::{create_membership_proof, verify_membership_proof};

/// Send protocol ALPN
pub const SEND_ALPN: &[u8] = b"harbor/send/0";

/// A read receipt acknowledging packet delivery
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipt {
    /// The packet being acknowledged
    pub packet_id: [u8; 32],
    /// The acknowledging node's EndpointID
    pub sender: [u8; 32],
}

impl Receipt {
    /// Create a new receipt
    pub fn new(packet_id: [u8; 32], sender: [u8; 32]) -> Self {
        Self { packet_id, sender }
    }
}

// =============================================================================
// Direct delivery messages (QUIC TLS only, no app-level crypto)
// =============================================================================

/// Direct delivery of a topic message (raw payload over QUIC TLS)
///
/// Used for direct peer-to-peer delivery when recipient is online.
/// QUIC TLS provides transport encryption - no app-level crypto needed.
///
/// Uses `harbor_id` (hash of topic_id) instead of raw topic_id to avoid
/// exposing topic membership on the wire. The `membership_proof` proves
/// the sender knows the actual topic_id without revealing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverTopic {
    /// Hash of topic_id - hides actual topic from wire
    pub harbor_id: [u8; 32],
    /// Cryptographic proof of topic membership: BLAKE3(topic_id || harbor_id || sender_id)
    pub membership_proof: [u8; 32],
    /// Raw encoded TopicMessage payload
    pub payload: Vec<u8>,
}

// Membership proof helpers are defined in network::membership and re-exported here
// for backward compatibility.

/// Direct delivery of a DM (raw payload over QUIC TLS)
///
/// Used for direct peer-to-peer delivery when recipient is online.
/// QUIC TLS provides transport encryption - no app-level crypto needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverDm {
    /// Raw encoded DmMessage payload
    pub payload: Vec<u8>,
}

/// Send RPC protocol definition using irpc
#[rpc_requests(message = SendRpcMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum SendRpcProtocol {
    /// Direct topic message delivery (QUIC TLS only, raw payload)
    #[rpc(tx = oneshot::Sender<Receipt>)]
    DeliverTopic(DeliverTopic),

    /// Direct DM delivery (QUIC TLS only, raw payload)
    #[rpc(tx = oneshot::Sender<Receipt>)]
    DeliverDm(DeliverDm),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_receipt_creation() {
        let receipt = Receipt::new([1u8; 32], [2u8; 32]);
        assert_eq!(receipt.packet_id, [1u8; 32]);
        assert_eq!(receipt.sender, [2u8; 32]);
    }

    #[test]
    fn test_receipt_serialization() {
        let receipt = Receipt::new([1u8; 32], [2u8; 32]);
        let bytes = postcard::to_allocvec(&receipt).unwrap();
        let decoded: Receipt = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn test_deliver_topic_serialization() {
        let topic_id = [42u8; 32];
        let sender_id = [1u8; 32];
        let harbor_id = crate::security::harbor_id_from_topic(&topic_id);
        let proof = create_membership_proof(&topic_id, &harbor_id, &sender_id);

        let deliver = DeliverTopic {
            harbor_id,
            membership_proof: proof,
            payload: b"Hello!".to_vec(),
        };
        let bytes = postcard::to_allocvec(&deliver).unwrap();
        let decoded: DeliverTopic = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.harbor_id, deliver.harbor_id);
        assert_eq!(decoded.membership_proof, deliver.membership_proof);
        assert_eq!(decoded.payload, deliver.payload);
    }

    #[test]
    fn test_membership_proof() {
        let topic_id = [42u8; 32];
        let sender_id = [1u8; 32];
        let harbor_id = crate::security::harbor_id_from_topic(&topic_id);
        let proof = create_membership_proof(&topic_id, &harbor_id, &sender_id);

        // Verify with correct values
        assert!(verify_membership_proof(&topic_id, &harbor_id, &sender_id, &proof));

        // Verify with wrong topic_id fails
        let wrong_topic = [99u8; 32];
        assert!(!verify_membership_proof(&wrong_topic, &harbor_id, &sender_id, &proof));

        // Verify with wrong sender_id fails
        let wrong_sender = [99u8; 32];
        assert!(!verify_membership_proof(&topic_id, &harbor_id, &wrong_sender, &proof));
    }

    #[test]
    fn test_deliver_dm_serialization() {
        let deliver = DeliverDm {
            payload: b"Hello DM!".to_vec(),
        };
        let bytes = postcard::to_allocvec(&deliver).unwrap();
        let decoded: DeliverDm = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.payload, deliver.payload);
    }
}
