//! Send protocol wire format
//!
//! Uses irpc for typed RPC over QUIC bi-streams:
//! - DeliverPacket: Send a packet, receive a Receipt as response
//!
//! This replaces the previous manual encode/decode with irpc's
//! varint length-prefix + postcard serialization.

use irpc::channel::oneshot;
use irpc::rpc_requests;
use serde::{Deserialize, Serialize};

use crate::security::SendPacket;

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

/// Deliver a packet to a peer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliverPacket {
    /// The packet to deliver
    pub packet: SendPacket,
}

/// Send RPC protocol definition using irpc
#[rpc_requests(message = SendRpcMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum SendRpcProtocol {
    /// Deliver a packet and receive a receipt
    #[rpc(tx = oneshot::Sender<Receipt>)]
    DeliverPacket(DeliverPacket),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{create_packet, generate_key_pair};

    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }

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
    fn test_deliver_packet_serialization() {
        let kp = generate_key_pair();
        let packet = create_packet(
            &test_topic_id(),
            &kp.private_key,
            &kp.public_key,
            b"Hello!",
        ).unwrap();

        let deliver = DeliverPacket { packet: packet.clone() };
        let bytes = postcard::to_allocvec(&deliver).unwrap();
        let decoded: DeliverPacket = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.packet.packet_id, packet.packet_id);
    }
}
