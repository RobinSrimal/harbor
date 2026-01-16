//! Send Service - manages packet delivery and receipts
//!
//! The SendService handles:
//! - Creating and sending packets to topic members
//! - Receiving packets and sending receipts
//! - Tracking delivery status
//! - Triggering Harbor replication when needed

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, NodeId, NodeAddr};
use zeroize::{Zeroize, ZeroizeOnDrop};
use iroh::endpoint::Connection;
use tokio::sync::RwLock;
use tracing::{debug, trace};

use crate::data::outgoing::{
    store_outgoing_packet, acknowledge_receipt as db_acknowledge_receipt,
};
use crate::network::harbor::protocol::HarborPacketType;
use crate::resilience::{PoWConfig, PoWChallenge, PoWVerifyResult, compute_pow, verify_pow};
use crate::security::{
    SendPacket, PacketError, create_packet, verify_and_decrypt_packet,
    harbor_id_from_topic, send_target_id,
};
use super::protocol::{SEND_ALPN, SendMessage, Receipt};

/// Configuration for the Send service
#[derive(Debug, Clone)]
pub struct SendConfig {
    /// Timeout for sending to a single recipient (default: 5 seconds)
    pub send_timeout: Duration,
    /// How long to wait for receipts before replicating to Harbor (default: 30 seconds)
    pub receipt_timeout: Duration,
    /// Maximum concurrent sends (default: 10)
    pub max_concurrent_sends: usize,
    /// Proof of Work configuration for receiving packets
    pub pow_config: PoWConfig,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            send_timeout: Duration::from_secs(5),
            receipt_timeout: Duration::from_secs(30),
            max_concurrent_sends: 10,
            pow_config: PoWConfig::default(),
        }
    }
}

impl SendConfig {
    /// Create config with PoW disabled (for testing)
    pub fn without_pow() -> Self {
        Self {
            pow_config: PoWConfig::disabled(),
            ..Default::default()
        }
    }
}

/// Error during Send operations
#[derive(Debug)]
pub enum SendError {
    /// Failed to create packet
    PacketCreation(PacketError),
    /// Failed to connect to recipient
    Connection(String),
    /// Failed to send data
    Send(String),
    /// Database error
    Database(String),
    /// No recipients
    NoRecipients,
    /// Proof of Work required but not provided
    PoWRequired,
    /// Invalid Proof of Work
    InvalidPoW(PoWVerifyResult),
    /// PoW target mismatch (wrong topic or recipient)
    PoWTargetMismatch,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::PacketCreation(e) => write!(f, "packet creation failed: {}", e),
            SendError::Connection(e) => write!(f, "connection failed: {}", e),
            SendError::Send(e) => write!(f, "send failed: {}", e),
            SendError::Database(e) => write!(f, "database error: {}", e),
            SendError::NoRecipients => write!(f, "no recipients specified"),
            SendError::PoWRequired => write!(f, "proof of work required"),
            SendError::InvalidPoW(result) => write!(f, "invalid proof of work: {}", result),
            SendError::PoWTargetMismatch => write!(f, "proof of work target mismatch"),
        }
    }
}

impl std::error::Error for SendError {}

impl From<PacketError> for SendError {
    fn from(e: PacketError) -> Self {
        SendError::PacketCreation(e)
    }
}

/// Result of sending a packet
#[derive(Debug, Clone)]
pub struct SendResult {
    /// The packet ID
    pub packet_id: [u8; 32],
    /// Recipients that were successfully reached
    pub delivered_to: Vec<[u8; 32]>,
    /// Recipients that failed
    pub failed: Vec<([u8; 32], String)>,
}

/// Callback for received packets
pub type PacketCallback = Box<dyn Fn(SendPacket, Vec<u8>) + Send + Sync>;

/// Send service for the Harbor protocol
/// 
/// The private key is zeroized on drop to prevent key material
/// from lingering in memory after the service is shut down.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SendService {
    /// Iroh endpoint
    #[zeroize(skip)]
    endpoint: Endpoint,
    /// Local node's private key (zeroized on drop)
    private_key: [u8; 32],
    /// Local node's public key (EndpointID)
    public_key: [u8; 32],
    /// Configuration
    #[zeroize(skip)]
    config: SendConfig,
    /// Active connections cache
    #[zeroize(skip)]
    connections: Arc<RwLock<HashMap<NodeId, Connection>>>,
}

impl SendService {
    /// Create a new Send service
    pub fn new(
        endpoint: Endpoint,
        private_key: [u8; 32],
        public_key: [u8; 32],
        config: SendConfig,
    ) -> Self {
        Self {
            endpoint,
            private_key,
            public_key,
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.public_key
    }

    /// Send a message to topic members
    ///
    /// # Arguments
    /// * `topic_id` - The topic to send on
    /// * `recipients` - EndpointIDs of topic members to send to
    /// * `plaintext` - The message payload
    /// * `conn` - Database connection for tracking (mutable for transaction)
    ///
    /// # Returns
    /// Result with delivery status for each recipient
    ///
    /// # PoW
    /// Computes a unique PoW for each recipient to prevent replay attacks.
    /// The PoW is bound to `send_target_id(topic_id, recipient_id) + packet_id + timestamp`.
    pub async fn send(
        &self,
        topic_id: &[u8; 32],
        recipients: &[[u8; 32]],
        plaintext: &[u8],
        conn: &mut rusqlite::Connection,
        packet_type: HarborPacketType,
    ) -> Result<SendResult, SendError> {
        if recipients.is_empty() {
            return Err(SendError::NoRecipients);
        }

        // Create the packet
        let packet = create_packet(
            topic_id,
            &self.private_key,
            &self.public_key,
            plaintext,
        )?;

        let packet_id = packet.packet_id;
        let harbor_id = harbor_id_from_topic(topic_id);
        let packet_bytes = packet.to_bytes()
            .map_err(SendError::PacketCreation)?;

        // Store in outgoing table for tracking
        store_outgoing_packet(
            conn,
            &packet_id,
            topic_id,
            &harbor_id,
            &packet_bytes,
            recipients,
            packet_type as u8,
        ).map_err(|e| SendError::Database(e.to_string()))?;

        let mut delivered_to = Vec::new();
        let mut failed = Vec::new();

        for recipient in recipients {
            // Skip self
            if *recipient == self.public_key {
                delivered_to.push(*recipient);
                continue;
            }

            // Compute PoW for this specific recipient
            // send_target_id = hash(topic_id || recipient_id)
            let target_id = send_target_id(topic_id, recipient);
            let challenge = PoWChallenge::new(target_id, packet_id, self.config.pow_config.difficulty_bits);
            let pow = compute_pow(&challenge);

            // Create message with PoW
            let message = SendMessage::PacketWithPoW {
                packet: packet.clone(),
                proof_of_work: pow,
            };
            let encoded = message.encode();

            match self.send_to_recipient(recipient, &encoded).await {
                Ok(()) => {
                    delivered_to.push(*recipient);
                    trace!(recipient = hex::encode(recipient), "packet delivered");
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    failed.push((*recipient, err_msg.clone()));
                    debug!(recipient = hex::encode(recipient), error = %err_msg, "delivery failed");
                }
            }
        }

        Ok(SendResult {
            packet_id,
            delivered_to,
            failed,
        })
    }

    /// Send encoded message to a single recipient
    async fn send_to_recipient(
        &self,
        recipient: &[u8; 32],
        encoded: &[u8],
    ) -> Result<(), SendError> {
        let node_id = NodeId::from_bytes(recipient)
            .map_err(|e| SendError::Connection(e.to_string()))?;

        // Get or create connection
        let conn = self.get_connection(node_id).await?;

        // Open unidirectional stream and send
        let mut send_stream = conn
            .open_uni()
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        send_stream
            .write_all(encoded)
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        send_stream
            .finish()
            .map_err(|e| SendError::Send(e.to_string()))?;

        Ok(())
    }

    /// Get or create a connection to a node
    async fn get_connection(&self, node_id: NodeId) -> Result<Connection, SendError> {
        // Check cache
        {
            let connections = self.connections.read().await;
            if let Some(conn) = connections.get(&node_id) {
                if conn.close_reason().is_none() {
                    return Ok(conn.clone());
                }
            }
        }

        // Create new connection
        let node_addr: NodeAddr = node_id.into();
        let conn = tokio::time::timeout(
            self.config.send_timeout,
            self.endpoint.connect(node_addr, SEND_ALPN),
        )
        .await
        .map_err(|_| SendError::Connection("timeout".to_string()))?
        .map_err(|e| SendError::Connection(e.to_string()))?;

        // Cache it
        {
            let mut connections = self.connections.write().await;
            connections.insert(node_id, conn.clone());
        }

        Ok(conn)
    }

    /// Process a received packet (legacy, no PoW verification)
    ///
    /// # Returns
    /// - The decrypted plaintext if verification succeeds
    /// - The receipt to send back
    ///
    /// # Note
    /// This method does not verify PoW. Use `receive_packet_with_pow` for
    /// spam-protected reception.
    pub fn receive_packet(
        &self,
        packet: &SendPacket,
        topic_id: &[u8; 32],
    ) -> Result<(Vec<u8>, Receipt), PacketError> {
        // Verify and decrypt
        let plaintext = verify_and_decrypt_packet(packet, topic_id)?;

        // Create receipt
        let receipt = Receipt::new(packet.packet_id, self.public_key);

        Ok((plaintext, receipt))
    }

    /// Process a received packet with PoW verification
    ///
    /// # Arguments
    /// * `packet` - The received packet
    /// * `proof_of_work` - The PoW attached to the packet
    /// * `topic_id` - The topic this packet is for
    ///
    /// # Returns
    /// - The decrypted plaintext if verification succeeds
    /// - The receipt to send back
    ///
    /// # PoW Verification
    /// Verifies that:
    /// 1. PoW target matches `send_target_id(topic_id, our_endpoint_id)`
    /// 2. PoW packet_id matches the packet's packet_id
    /// 3. PoW meets difficulty and freshness requirements
    pub fn receive_packet_with_pow(
        &self,
        packet: &SendPacket,
        proof_of_work: &crate::resilience::ProofOfWork,
        topic_id: &[u8; 32],
    ) -> Result<(Vec<u8>, Receipt), SendError> {
        // Skip PoW verification if disabled
        if !self.config.pow_config.enabled {
            let (plaintext, receipt) = self.receive_packet(packet, topic_id)
                .map_err(SendError::PacketCreation)?;
            return Ok((plaintext, receipt));
        }

        // Verify PoW is bound to correct target (topic + us as recipient)
        let expected_target = send_target_id(topic_id, &self.public_key);
        if proof_of_work.harbor_id != expected_target {
            return Err(SendError::PoWTargetMismatch);
        }

        // Verify PoW is bound to this packet
        if proof_of_work.packet_id != packet.packet_id {
            return Err(SendError::PoWTargetMismatch);
        }

        // Verify PoW meets requirements (difficulty, freshness)
        let result = verify_pow(proof_of_work, &self.config.pow_config);
        if !result.is_valid() {
            return Err(SendError::InvalidPoW(result));
        }

        // PoW valid - now verify and decrypt the packet
        let plaintext = verify_and_decrypt_packet(packet, topic_id)
            .map_err(SendError::PacketCreation)?;

        // Create receipt
        let receipt = Receipt::new(packet.packet_id, self.public_key);

        Ok((plaintext, receipt))
    }

    /// Send a receipt back to the sender
    pub async fn send_receipt(
        &self,
        receipt: &Receipt,
        sender: &[u8; 32],
    ) -> Result<(), SendError> {
        let message = SendMessage::Receipt(receipt.clone());
        let encoded = message.encode();
        self.send_to_recipient(sender, &encoded).await
    }

    /// Process a received receipt
    /// 
    /// Note: Takes `&Connection` (not `&mut`) because this is a single UPDATE
    /// operation that doesn't require transactional guarantees. The `send()`
    /// method takes `&mut Connection` because it inserts into multiple tables
    /// atomically.
    pub fn process_receipt(
        &self,
        receipt: &Receipt,
        conn: &rusqlite::Connection,
    ) -> Result<bool, SendError> {
        db_acknowledge_receipt(conn, &receipt.packet_id, &receipt.sender)
            .map_err(|e| SendError::Database(e.to_string()))
    }

    /// Close connection to a node
    pub async fn close_connection(&self, node_id: NodeId) {
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.remove(&node_id) {
            conn.close(0u32.into(), b"close");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::create_key_pair::generate_key_pair;
    use crate::security::send::packet::create_packet;
    use crate::data::schema::create_all_tables;

    #[allow(dead_code)]
    fn test_config() -> SendConfig {
        SendConfig {
            send_timeout: Duration::from_millis(100),
            receipt_timeout: Duration::from_millis(500),
            max_concurrent_sends: 5,
            pow_config: crate::resilience::PoWConfig::disabled(),
        }
    }

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
    fn test_send_config_default() {
        let config = SendConfig::default();
        assert_eq!(config.send_timeout, Duration::from_secs(5));
        assert_eq!(config.receipt_timeout, Duration::from_secs(30));
        assert_eq!(config.max_concurrent_sends, 10);
        assert!(config.pow_config.enabled); // PoW enabled by default
    }

    #[test]
    fn test_send_config_without_pow() {
        let config = SendConfig::without_pow();
        assert!(!config.pow_config.enabled);
    }

    #[test]
    fn test_send_error_display() {
        let err = SendError::NoRecipients;
        assert_eq!(err.to_string(), "no recipients specified");

        let err = SendError::Connection("test".to_string());
        assert_eq!(err.to_string(), "connection failed: test");

        let err = SendError::Send("stream closed".to_string());
        assert_eq!(err.to_string(), "send failed: stream closed");

        let err = SendError::Database("constraint failed".to_string());
        assert_eq!(err.to_string(), "database error: constraint failed");

        let err = SendError::PoWRequired;
        assert_eq!(err.to_string(), "proof of work required");

        let err = SendError::PoWTargetMismatch;
        assert_eq!(err.to_string(), "proof of work target mismatch");

        let err = SendError::InvalidPoW(crate::resilience::PoWVerifyResult::Expired);
        assert!(err.to_string().contains("invalid proof of work"));
    }

    #[test]
    fn test_send_error_from_packet_error() {
        let packet_err = PacketError::InvalidMac;
        let send_err: SendError = packet_err.into();
        assert!(matches!(send_err, SendError::PacketCreation(_)));
    }

    #[test]
    fn test_send_result_structure() {
        let result = SendResult {
            packet_id: [1u8; 32],
            delivered_to: vec![test_id(1), test_id(2)],
            failed: vec![(test_id(3), "timeout".to_string())],
        };

        assert_eq!(result.packet_id, [1u8; 32]);
        assert_eq!(result.delivered_to.len(), 2);
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].1, "timeout");
    }

    #[test]
    fn test_receive_packet_success() {
        // Create sender's keys
        let sender_keys = generate_key_pair();
        let topic_id = test_id(100);

        // Create a packet
        let plaintext = b"Hello, world!";
        let packet = create_packet(
            &topic_id,
            &sender_keys.private_key,
            &sender_keys.public_key,
            plaintext,
        ).unwrap();

        // Create receiver (for testing, we just need any key pair)
        let receiver_keys = generate_key_pair();

        // Simulate receive_packet logic directly (without SendService)
        // since we can't create an Endpoint in tests
        use crate::security::verify_and_decrypt_packet;
        let decrypted = verify_and_decrypt_packet(&packet, &topic_id).unwrap();
        assert_eq!(decrypted, plaintext);

        // Create receipt
        let receipt = Receipt::new(packet.packet_id, receiver_keys.public_key);
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
        ).unwrap();

        // Try to decrypt with wrong topic should fail
        use crate::security::verify_and_decrypt_packet;
        let result = verify_and_decrypt_packet(&packet, &wrong_topic_id);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_receipt_success() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [42u8; 32];
        let _sender_id = test_id(1);
        let recipient_id = test_id(2);

        // Store outgoing packet
        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"packet data",
            &[recipient_id],
            0, // Content
        ).unwrap();

        // Process receipt
        let receipt = Receipt::new(packet_id, recipient_id);
        let acked = db_acknowledge_receipt(&db_conn, &receipt.packet_id, &receipt.sender).unwrap();
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
            0, // Content
        ).unwrap();

        // First ack
        let receipt = Receipt::new(packet_id, recipient_id);
        let acked1 = db_acknowledge_receipt(&db_conn, &receipt.packet_id, &receipt.sender).unwrap();
        assert!(acked1);

        // Second ack should return false (already acked)
        let acked2 = db_acknowledge_receipt(&db_conn, &receipt.packet_id, &receipt.sender).unwrap();
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

    #[test]
    fn test_outgoing_packet_tracking() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);
        let packet_id = [1u8; 32];
        let recipients = vec![test_id(10), test_id(11), test_id(12)];

        // Store packet
        store_outgoing_packet(
            &mut db_conn,
            &packet_id,
            &topic_id,
            &harbor_id,
            b"data",
            &recipients,
            0, // Content
        ).unwrap();

        // Get packets needing replication (none acked yet)
        use crate::data::outgoing::get_packets_needing_replication;
        let needs_replication = get_packets_needing_replication(&db_conn).unwrap();
        assert_eq!(needs_replication.len(), 1);

        // Ack all recipients
        for r in &recipients {
            db_acknowledge_receipt(&db_conn, &packet_id, r).unwrap();
        }

        // Should no longer need replication
        let needs_replication = get_packets_needing_replication(&db_conn).unwrap();
        assert!(needs_replication.is_empty());
    }

    #[test]
    fn test_multiple_packets_tracking() {
        let mut db_conn = setup_db();
        let topic_id = test_id(100);
        let harbor_id = harbor_id_from_topic(&topic_id);

        // Store multiple packets
        for i in 0..5 {
            let packet_id = [i; 32];
            store_outgoing_packet(
                &mut db_conn,
                &packet_id,
                &topic_id,
                &harbor_id,
                b"data",
                &[test_id(10 + i)],
                0, // Content
            ).unwrap();
        }

        use crate::data::outgoing::get_packets_needing_replication;
        let needs_replication = get_packets_needing_replication(&db_conn).unwrap();
        assert_eq!(needs_replication.len(), 5);
    }

    // Full network tests are in src/testing/network.rs
    // using the offline testing framework
}

