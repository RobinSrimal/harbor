//! Send protocol wire format
//!
//! Simple framing for Send messages without irpc overhead.
//!
//! Includes Proof of Work to prevent spam:
//! - PoW is bound to `send_target_id + packet_id + timestamp`
//! - `send_target_id = hash(topic_id || recipient_id)`
//! - Prevents cross-topic and cross-recipient replay attacks

use serde::{Deserialize, Serialize};

use crate::resilience::ProofOfWork;
use crate::security::SendPacket;

/// Send protocol ALPN
pub const SEND_ALPN: &[u8] = b"harbor/send/0";

/// Message type byte
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// A Send packet (legacy, no PoW)
    Packet = 0x01,
    /// A read receipt
    Receipt = 0x02,
    /// A Send packet with Proof of Work
    PacketWithPoW = 0x03,
}

impl TryFrom<u8> for MessageType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(MessageType::Packet),
            0x02 => Ok(MessageType::Receipt),
            0x03 => Ok(MessageType::PacketWithPoW),
            _ => Err(()),
        }
    }
}

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

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(&self.packet_id);
        bytes.extend_from_slice(&self.sender);
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 64 {
            return None;
        }
        let mut packet_id = [0u8; 32];
        let mut sender = [0u8; 32];
        packet_id.copy_from_slice(&bytes[..32]);
        sender.copy_from_slice(&bytes[32..64]);
        Some(Self { packet_id, sender })
    }
}

/// A message in the Send protocol
#[derive(Debug, Clone)]
pub enum SendMessage {
    /// A Send packet to deliver (legacy, no PoW - may be rejected by receivers)
    Packet(SendPacket),
    /// A read receipt
    Receipt(Receipt),
    /// A Send packet with Proof of Work
    PacketWithPoW {
        /// The packet to deliver
        packet: SendPacket,
        /// Proof of Work bound to send_target_id + packet_id + timestamp
        proof_of_work: ProofOfWork,
    },
}

impl SendMessage {
    /// Encode message for transmission
    pub fn encode(&self) -> Vec<u8> {
        match self {
            SendMessage::Packet(packet) => {
                // to_bytes() can only fail if postcard serialization fails,
                // which should never happen for a valid in-memory packet
                let packet_bytes = packet.to_bytes()
                    .expect("packet serialization should not fail");
                let len = packet_bytes.len() as u32;
                
                let mut bytes = Vec::with_capacity(5 + packet_bytes.len());
                bytes.push(MessageType::Packet as u8);
                bytes.extend_from_slice(&len.to_be_bytes());
                bytes.extend_from_slice(&packet_bytes);
                bytes
            }
            SendMessage::Receipt(receipt) => {
                let mut bytes = Vec::with_capacity(65);
                bytes.push(MessageType::Receipt as u8);
                bytes.extend_from_slice(&receipt.to_bytes());
                bytes
            }
            SendMessage::PacketWithPoW { packet, proof_of_work } => {
                let packet_bytes = packet.to_bytes()
                    .expect("packet serialization should not fail");
                let pow_bytes = postcard::to_allocvec(proof_of_work)
                    .expect("PoW serialization should not fail");
                
                // Format: type(1) + packet_len(4) + packet + pow_len(4) + pow
                let mut bytes = Vec::with_capacity(9 + packet_bytes.len() + pow_bytes.len());
                bytes.push(MessageType::PacketWithPoW as u8);
                bytes.extend_from_slice(&(packet_bytes.len() as u32).to_be_bytes());
                bytes.extend_from_slice(&packet_bytes);
                bytes.extend_from_slice(&(pow_bytes.len() as u32).to_be_bytes());
                bytes.extend_from_slice(&pow_bytes);
                bytes
            }
        }
    }

    /// Decode message from bytes
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError::Empty);
        }

        let msg_type = MessageType::try_from(bytes[0])
            .map_err(|_| DecodeError::UnknownType(bytes[0]))?;

        match msg_type {
            MessageType::Packet => {
                if bytes.len() < 5 {
                    return Err(DecodeError::TooShort);
                }
                let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                if bytes.len() < 5 + len {
                    return Err(DecodeError::TooShort);
                }
                let packet = SendPacket::from_bytes(&bytes[5..5 + len])
                    .map_err(|e| DecodeError::InvalidPacket(e.to_string()))?;
                Ok(SendMessage::Packet(packet))
            }
            MessageType::Receipt => {
                if bytes.len() < 65 {
                    return Err(DecodeError::TooShort);
                }
                let receipt = Receipt::from_bytes(&bytes[1..65])
                    .ok_or(DecodeError::InvalidReceipt)?;
                Ok(SendMessage::Receipt(receipt))
            }
            MessageType::PacketWithPoW => {
                // Format: type(1) + packet_len(4) + packet + pow_len(4) + pow
                if bytes.len() < 9 {
                    return Err(DecodeError::TooShort);
                }
                let packet_len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
                if bytes.len() < 5 + packet_len + 4 {
                    return Err(DecodeError::TooShort);
                }
                let packet = SendPacket::from_bytes(&bytes[5..5 + packet_len])
                    .map_err(|e| DecodeError::InvalidPacket(e.to_string()))?;
                
                let pow_start = 5 + packet_len;
                let pow_len = u32::from_be_bytes([
                    bytes[pow_start],
                    bytes[pow_start + 1],
                    bytes[pow_start + 2],
                    bytes[pow_start + 3],
                ]) as usize;
                
                if bytes.len() < pow_start + 4 + pow_len {
                    return Err(DecodeError::TooShort);
                }
                let proof_of_work: ProofOfWork = postcard::from_bytes(&bytes[pow_start + 4..pow_start + 4 + pow_len])
                    .map_err(|e| DecodeError::InvalidPacket(format!("invalid PoW: {}", e)))?;
                
                Ok(SendMessage::PacketWithPoW { packet, proof_of_work })
            }
        }
    }
}

/// Error decoding a message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Empty input
    Empty,
    /// Unknown message type
    UnknownType(u8),
    /// Message too short
    TooShort,
    /// Invalid packet data
    InvalidPacket(String),
    /// Invalid receipt data
    InvalidReceipt,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Empty => write!(f, "empty message"),
            DecodeError::UnknownType(t) => write!(f, "unknown message type: {}", t),
            DecodeError::TooShort => write!(f, "message too short"),
            DecodeError::InvalidPacket(e) => write!(f, "invalid packet: {}", e),
            DecodeError::InvalidReceipt => write!(f, "invalid receipt"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{create_packet, generate_key_pair};

    fn test_topic_id() -> [u8; 32] {
        [42u8; 32]
    }

    #[test]
    fn test_receipt_roundtrip() {
        let receipt = Receipt::new([1u8; 32], [2u8; 32]);
        let bytes = receipt.to_bytes();
        let restored = Receipt::from_bytes(&bytes).unwrap();
        
        assert_eq!(restored, receipt);
    }

    #[test]
    fn test_receipt_wrong_length() {
        assert!(Receipt::from_bytes(&[0u8; 63]).is_none());
        assert!(Receipt::from_bytes(&[0u8; 65]).is_none());
    }

    #[test]
    fn test_message_packet_roundtrip() {
        let kp = generate_key_pair();
        let packet = create_packet(
            &test_topic_id(),
            &kp.private_key,
            &kp.public_key,
            b"Hello!",
        ).unwrap();

        let msg = SendMessage::Packet(packet.clone());
        let encoded = msg.encode();
        let decoded = SendMessage::decode(&encoded).unwrap();

        match decoded {
            SendMessage::Packet(p) => {
                assert_eq!(p.packet_id, packet.packet_id);
                assert_eq!(p.endpoint_id, packet.endpoint_id);
            }
            _ => panic!("expected packet"),
        }
    }

    #[test]
    fn test_message_receipt_roundtrip() {
        let receipt = Receipt::new([1u8; 32], [2u8; 32]);
        let msg = SendMessage::Receipt(receipt.clone());
        let encoded = msg.encode();
        let decoded = SendMessage::decode(&encoded).unwrap();

        match decoded {
            SendMessage::Receipt(r) => assert_eq!(r, receipt),
            _ => panic!("expected receipt"),
        }
    }

    #[test]
    fn test_decode_empty() {
        assert!(matches!(
            SendMessage::decode(&[]),
            Err(DecodeError::Empty)
        ));
    }

    #[test]
    fn test_decode_unknown_type() {
        assert!(matches!(
            SendMessage::decode(&[0xFF]),
            Err(DecodeError::UnknownType(0xFF))
        ));
    }

    #[test]
    fn test_decode_too_short() {
        // Packet type but no length
        assert!(matches!(
            SendMessage::decode(&[0x01]),
            Err(DecodeError::TooShort)
        ));

        // Receipt type but too short
        assert!(matches!(
            SendMessage::decode(&[0x02, 0, 0]),
            Err(DecodeError::TooShort)
        ));
    }

    #[test]
    fn test_message_type_conversion() {
        assert_eq!(MessageType::try_from(0x01), Ok(MessageType::Packet));
        assert_eq!(MessageType::try_from(0x02), Ok(MessageType::Receipt));
        assert_eq!(MessageType::try_from(0x03), Ok(MessageType::PacketWithPoW));
        assert!(MessageType::try_from(0x00).is_err());
        assert!(MessageType::try_from(0x04).is_err());
    }

    #[test]
    fn test_message_packet_with_pow_roundtrip() {
        use crate::resilience::ProofOfWork;
        
        let kp = generate_key_pair();
        let packet = create_packet(
            &test_topic_id(),
            &kp.private_key,
            &kp.public_key,
            b"Hello with PoW!",
        ).unwrap();

        let pow = ProofOfWork {
            harbor_id: [1u8; 32], // send_target_id in our case
            packet_id: packet.packet_id,
            timestamp: 12345,
            nonce: 67890,
        };

        let msg = SendMessage::PacketWithPoW {
            packet: packet.clone(),
            proof_of_work: pow.clone(),
        };
        let encoded = msg.encode();
        let decoded = SendMessage::decode(&encoded).unwrap();

        match decoded {
            SendMessage::PacketWithPoW { packet: p, proof_of_work: pow_dec } => {
                assert_eq!(p.packet_id, packet.packet_id);
                assert_eq!(pow_dec.nonce, pow.nonce);
                assert_eq!(pow_dec.timestamp, pow.timestamp);
            }
            _ => panic!("expected PacketWithPoW"),
        }
    }
}

