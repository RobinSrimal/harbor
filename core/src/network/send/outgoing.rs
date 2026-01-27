//! Send Service - Outgoing packet delivery
//!
//! This module handles sending packets to topic members:
//! - Creating and encrypting packets
//! - Computing per-recipient PoW
//! - Parallel delivery to multiple recipients
//! - Tracking delivery status
//! - Connection management

use std::collections::HashMap;
use std::sync::Arc;

use iroh::{Endpoint, EndpointId, EndpointAddr};
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, trace};

use crate::data::send::store_outgoing_packet;
use crate::data::LocalIdentity;
use crate::network::harbor::protocol::HarborPacketType;
use crate::protocol::MemberInfo;
use crate::resilience::{PoWChallenge, compute_pow};
use crate::security::{
    PacketError, create_packet, harbor_id_from_topic, send_target_id,
};
use super::protocol::{SEND_ALPN, SendMessage, Receipt};
use super::SendConfig;

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
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::PacketCreation(e) => write!(f, "packet creation failed: {}", e),
            SendError::Connection(e) => write!(f, "connection failed: {}", e),
            SendError::Send(e) => write!(f, "send failed: {}", e),
            SendError::Database(e) => write!(f, "database error: {}", e),
            SendError::NoRecipients => write!(f, "no recipients specified"),
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

/// Send service for outgoing packets
///
/// Follows the standard service pattern:
/// - Takes Arc<LocalIdentity> for signing/encryption
/// - Takes Arc<Mutex<DbConnection>> for persistence
/// - Takes Endpoint for network operations
pub struct SendService {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Local identity (shared with Protocol)
    identity: Arc<LocalIdentity>,
    /// Database connection (shared with Protocol)
    db: Arc<Mutex<DbConnection>>,
    /// Configuration
    config: SendConfig,
    /// Active connections cache
    connections: Arc<RwLock<HashMap<EndpointId, Connection>>>, 
    /// TODO REMOVE THIS!!!!!!
}

impl SendService {
    /// Create a new Send service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        config: SendConfig,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            config,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get our EndpointID
    pub fn endpoint_id(&self) -> [u8; 32] {
        self.identity.public_key
    }

    /// Get the identity (for signing/encryption operations)
    pub fn identity(&self) -> &LocalIdentity {
        &self.identity
    }

    /// Get the PoW configuration
    pub fn pow_config(&self) -> &crate::resilience::PoWConfig {
        &self.config.pow_config
    }

    /// Send a message to topic members
    ///
    /// # Arguments
    /// * `topic_id` - The topic to send on
    /// * `recipients` - MemberInfo for topic members to send to (includes relay URLs)
    /// * `plaintext` - The message payload
    /// * `packet_type` - Type of packet (Content, SyncUpdate, etc.)
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
        recipients: &[MemberInfo],
        plaintext: &[u8],
        packet_type: HarborPacketType,
    ) -> Result<SendResult, SendError> {
        if recipients.is_empty() {
            return Err(SendError::NoRecipients);
        }

        // Extract endpoint IDs for storage
        let recipient_ids: Vec<[u8; 32]> = recipients.iter().map(|m| m.endpoint_id).collect();

        // Create the packet
        let packet = create_packet(
            topic_id,
            &self.identity.private_key,
            &self.identity.public_key,
            plaintext,
        )?;

        let packet_id = packet.packet_id;
        let harbor_id = harbor_id_from_topic(topic_id);
        let packet_bytes = packet.to_bytes()
            .map_err(SendError::PacketCreation)?;

        // Store in outgoing table for tracking
        {
            let mut db = self.db.lock().await;
            store_outgoing_packet(
                &mut db,
                &packet_id,
                topic_id,
                &harbor_id,
                &packet_bytes,
                &recipient_ids,
                packet_type as u8,
            ).map_err(|e| SendError::Database(e.to_string()))?;
        }

        let mut delivered_to = Vec::new();
        let mut failed = Vec::new();

        for member in recipients {
            // Skip self
            if member.endpoint_id == self.identity.public_key {
                delivered_to.push(member.endpoint_id);
                continue;
            }

            // Compute PoW for this specific recipient
            // send_target_id = hash(topic_id || recipient_id)
            let target_id = send_target_id(topic_id, &member.endpoint_id);
            let challenge = PoWChallenge::new(target_id, packet_id, self.config.pow_config.difficulty_bits);
            let pow = compute_pow(&challenge);

            // Create message with PoW
            let message = SendMessage::PacketWithPoW {
                packet: packet.clone(),
                proof_of_work: pow,
            };
            let encoded = message.encode();

            match self.send_to_member(member, &encoded).await {
                Ok(()) => {
                    delivered_to.push(member.endpoint_id);
                    trace!(recipient = hex::encode(&member.endpoint_id), "packet delivered");
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    failed.push((member.endpoint_id, err_msg.clone()));
                    debug!(recipient = hex::encode(&member.endpoint_id), error = %err_msg, "delivery failed");
                }
            }
        }

        Ok(SendResult {
            packet_id,
            delivered_to,
            failed,
        })
    }

    /// Send encoded message to a single member (with relay URL support)
    ///
    /// This is a low-level transport function that handles:
    /// - Building EndpointAddr with relay URL if available
    /// - Getting/creating connections from the cache
    /// - Sending data over a unidirectional QUIC stream
    pub async fn send_to_member(
        &self,
        member: &MemberInfo,
        encoded: &[u8],
    ) -> Result<(), SendError> {
        let node_id = EndpointId::from_bytes(&member.endpoint_id)
            .map_err(|e| SendError::Connection(e.to_string()))?;

        // Build EndpointAddr with relay URL if available
        let node_addr = if let Some(ref relay_url) = member.relay_url {
            if let Ok(relay) = relay_url.parse::<iroh::RelayUrl>() {
                EndpointAddr::new(node_id).with_relay_url(relay)
            } else {
                EndpointAddr::from(node_id)
            }
        } else {
            EndpointAddr::from(node_id)
        };

        // Get or create connection
        let conn = self.get_connection(node_id, node_addr).await?;

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
    async fn get_connection(&self, node_id: EndpointId, node_addr: EndpointAddr) -> Result<Connection, SendError> {
        // Check cache
        {
            let connections = self.connections.read().await;
            if let Some(conn) = connections.get(&node_id) {
                if conn.close_reason().is_none() {
                    return Ok(conn.clone());
                }
            }
        }

        // Create new connection using provided EndpointAddr (which may include relay URL)
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

    /// Send a receipt back to the sender
    pub async fn send_receipt(
        &self,
        receipt: &Receipt,
        sender: &[u8; 32],
    ) -> Result<(), SendError> {
        let message = SendMessage::Receipt(receipt.clone());
        let encoded = message.encode();
        // Create MemberInfo without relay (receipts go to whoever sent us the packet)
        let member = MemberInfo::new(*sender);
        self.send_to_member(&member, &encoded).await
    }

    /// Close connection to a node
    pub async fn close_connection(&self, node_id: EndpointId) {
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.remove(&node_id) {
            conn.close(0u32.into(), b"close");
        }
    }
}

/// Prepare a packet for sending - creates, stores, and encodes
///
/// This function handles the business logic of packet preparation:
/// 1. Creates the packet (encrypt, MAC, sign)
/// 2. Stores in the outgoing table for tracking
/// 3. Encodes for wire transmission
///
/// The caller is responsible for the actual transport (sending to recipients).
///
/// # Arguments
/// * `topic_id` - The topic to send on
/// * `payload` - The message payload (plaintext)
/// * `recipients` - EndpointIDs of recipients (for tracking)
/// * `packet_type` - Type of packet (Content, SyncUpdate, etc.)
/// * `private_key` - Sender's private key for signing
/// * `public_key` - Sender's public key (EndpointID)
/// * `db` - Database connection for storing outgoing packet
///
/// # Returns
/// Tuple of (packet_id, encoded_message) on success
pub fn prepare_packet(
    topic_id: &[u8; 32],
    payload: &[u8],
    recipients: &[[u8; 32]],
    packet_type: crate::network::harbor::protocol::HarborPacketType,
    private_key: &[u8; 32],
    public_key: &[u8; 32],
    db: &mut rusqlite::Connection,
) -> Result<([u8; 32], Vec<u8>), SendError> {
    use crate::network::send::protocol::SendMessage;

    // Create the packet
    let packet = create_packet(topic_id, private_key, public_key, payload)?;

    let packet_id = packet.packet_id;
    let harbor_id = harbor_id_from_topic(topic_id);
    let packet_bytes = packet.to_bytes().map_err(SendError::PacketCreation)?;

    // Store in outgoing table for tracking
    store_outgoing_packet(
        db,
        &packet_id,
        topic_id,
        &harbor_id,
        &packet_bytes,
        recipients,
        packet_type as u8,
    )
    .map_err(|e| SendError::Database(e.to_string()))?;

    // Encode for wire
    let message = SendMessage::Packet(packet);
    let encoded = message.encode();

    Ok((packet_id, encoded))
}

/// Result of parallel send operation
#[derive(Debug, Clone)]
pub struct ParallelSendResult {
    /// Number of successful deliveries
    pub delivered: usize,
    /// Number of failed deliveries
    pub failed: usize,
}

impl ParallelSendResult {
    /// Returns true if all sends failed
    pub fn all_failed(&self) -> bool {
        self.delivered == 0 && self.failed > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::schema::create_all_tables;
    use crate::data::send::get_packets_needing_replication;

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
    fn test_send_error_display() {
        let err = SendError::NoRecipients;
        assert_eq!(err.to_string(), "no recipients specified");

        let err = SendError::Connection("test".to_string());
        assert_eq!(err.to_string(), "connection failed: test");

        let err = SendError::Send("stream closed".to_string());
        assert_eq!(err.to_string(), "send failed: stream closed");

        let err = SendError::Database("constraint failed".to_string());
        assert_eq!(err.to_string(), "database error: constraint failed");
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
        let needs_replication = get_packets_needing_replication(&db_conn).unwrap();
        assert_eq!(needs_replication.len(), 1);
    }
}
