//! Send Service - Incoming packet processing types
//!
//! Type definitions for incoming packet processing. The actual logic
//! lives on `SendService` methods in `outgoing.rs`.

use crate::security::PacketError;
use super::protocol::Receipt;

/// Error during incoming packet processing
#[derive(Debug)]
pub enum ProcessError {
    /// Decryption/verification failed (try next topic)
    VerificationFailed(String),
    /// Database error
    Database(String),
    /// Invalid message format
    InvalidMessage(String),
    /// Sender validation failed (e.g., joiner != sender)
    SenderMismatch(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::VerificationFailed(e) => write!(f, "verification failed: {}", e),
            ProcessError::Database(e) => write!(f, "database error: {}", e),
            ProcessError::InvalidMessage(e) => write!(f, "invalid message: {}", e),
            ProcessError::SenderMismatch(e) => write!(f, "sender mismatch: {}", e),
        }
    }
}

impl std::error::Error for ProcessError {}

/// Source of an incoming packet — determines processing behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketSource {
    /// Received via live QUIC connection (sender is online)
    Direct,
    /// Pulled from harbor storage (sender may be offline)
    HarborPull,
}

/// Result of processing an incoming packet
#[derive(Debug)]
pub struct ProcessResult {
    /// The receipt to send back
    pub receipt: Receipt,
    /// Whether the packet was a Content message (should be forwarded to app)
    pub content_payload: Option<Vec<u8>>,
}

/// Error during receive operations
#[derive(Debug)]
pub enum ReceiveError {
    /// Failed to verify/decrypt packet
    PacketVerification(PacketError),
}

impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiveError::PacketVerification(e) => write!(f, "packet verification failed: {}", e),
        }
    }
}

impl std::error::Error for ReceiveError {}

impl From<PacketError> for ReceiveError {
    fn from(e: PacketError) -> Self {
        ReceiveError::PacketVerification(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::{create_packet, generate_packet_id};
    use crate::data::schema::create_all_tables;
    use crate::data::send::store_outgoing_packet;
    use crate::security::harbor_id_from_topic;
    use crate::network::send::SendService;

    fn test_id(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    fn setup_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        create_all_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_process_error_display() {
        let err = ProcessError::VerificationFailed("bad MAC".to_string());
        assert!(err.to_string().contains("verification failed"));

        let err = ProcessError::Database("constraint".to_string());
        assert!(err.to_string().contains("database error"));

        let err = ProcessError::InvalidMessage("bad format".to_string());
        assert!(err.to_string().contains("invalid message"));

        let err = ProcessError::SenderMismatch("wrong sender".to_string());
        assert!(err.to_string().contains("sender mismatch"));
    }

    #[test]
    fn test_receive_packet_success() {
        let sender_keys = generate_key_pair();
        let topic_id = test_id(100);

        let plaintext = b"Hello, world!";
        let packet = create_packet(
            &topic_id,
            &sender_keys.private_key,
            &sender_keys.public_key,
            plaintext,
            generate_packet_id(),
        ).unwrap();

        let receiver_keys = generate_key_pair();

        let (decrypted, receipt) = SendService::receive_packet(&packet, &topic_id, receiver_keys.public_key).unwrap();
        assert_eq!(decrypted, plaintext);
        assert_eq!(receipt.packet_id, packet.packet_id);
        assert_eq!(receipt.sender, receiver_keys.public_key);
    }

    #[test]
    fn test_receive_packet_wrong_topic() {
        let sender_keys = generate_key_pair();
        let topic_id = test_id(100);
        let wrong_topic_id = test_id(200);

        let packet = create_packet(
            &topic_id,
            &sender_keys.private_key,
            &sender_keys.public_key,
            b"Secret",
            generate_packet_id(),
        ).unwrap();

        let result = SendService::receive_packet(&packet, &wrong_topic_id, [0u8; 32]);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_receipt_success() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let recipient_id = test_id(2);

        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0,
        ).unwrap();

        let receipt = Receipt::new(packet_id, recipient_id);
        let acked = SendService::process_receipt(&receipt, &db_conn).unwrap();
        assert!(acked);
    }

    #[test]
    fn test_process_receipt_already_acked() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let recipient_id = test_id(2);

        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0,
        ).unwrap();

        let receipt = Receipt::new(packet_id, recipient_id);
        let acked1 = SendService::process_receipt(&receipt, &db_conn).unwrap();
        assert!(acked1);

        let acked2 = SendService::process_receipt(&receipt, &db_conn).unwrap();
        assert!(!acked2);
    }

    #[test]
    fn test_receipt_creation() {
        let packet_id = [99u8; 32];
        let sender = test_id(5);

        let receipt = Receipt::new(packet_id, sender);
        assert_eq!(receipt.packet_id, packet_id);
        assert_eq!(receipt.sender, sender);
    }
}
