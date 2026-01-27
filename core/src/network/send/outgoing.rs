//! Send Service - The single entry point for all send operations
//!
//! Handles:
//! - Creating and encrypting packets
//! - Parallel delivery to multiple recipients via irpc RPC
//! - Inline receipt acknowledgement
//! - Connection management
//! - Incoming connection handling (via handle_send_connection)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use iroh::{Endpoint, EndpointId, EndpointAddr};
use iroh::endpoint::Connection;
use rusqlite::Connection as DbConnection;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, info, trace};

use crate::data::send::store_outgoing_packet;
use crate::data::{get_topic_members_with_info, LocalIdentity, WILDCARD_RECIPIENT};
use crate::network::harbor::protocol::HarborPacketType;
use crate::network::rpc::ExistingConnection;
use crate::protocol::{MemberInfo, ProtocolEvent};
use crate::security::{
    PacketError, create_packet, harbor_id_from_topic,
};
use super::protocol::{SEND_ALPN, Receipt, DeliverPacket, SendRpcProtocol};

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
    /// Topic not found
    TopicNotFound,
    /// Not a member of the topic
    NotMember,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::PacketCreation(e) => write!(f, "packet creation failed: {}", e),
            SendError::Connection(e) => write!(f, "connection failed: {}", e),
            SendError::Send(e) => write!(f, "send failed: {}", e),
            SendError::Database(e) => write!(f, "database error: {}", e),
            SendError::NoRecipients => write!(f, "no recipients specified"),
            SendError::TopicNotFound => write!(f, "topic not found"),
            SendError::NotMember => write!(f, "not a member of this topic"),
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

/// Send service - single entry point for all send operations
///
/// Owns connection cache, identity, and provides both outgoing
/// (send_to_topic) and incoming (handle_send_connection) operations.
pub struct SendService {
    /// Iroh endpoint
    endpoint: Endpoint,
    /// Local identity
    identity: Arc<LocalIdentity>,
    /// Database connection
    db: Arc<Mutex<DbConnection>>,
    /// Event sender (for incoming packet processing)
    event_tx: mpsc::Sender<ProtocolEvent>,
    /// Connection timeout
    send_timeout: Duration,
    /// Active connections cache
    connections: Arc<RwLock<HashMap<EndpointId, Connection>>>,
}

impl SendService {
    /// Create a new Send service
    pub fn new(
        endpoint: Endpoint,
        identity: Arc<LocalIdentity>,
        db: Arc<Mutex<DbConnection>>,
        event_tx: mpsc::Sender<ProtocolEvent>,
    ) -> Self {
        Self {
            endpoint,
            identity,
            db,
            event_tx,
            send_timeout: Duration::from_secs(5),
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

    /// Get the event sender
    pub fn event_tx(&self) -> &mpsc::Sender<ProtocolEvent> {
        &self.event_tx
    }

    /// Get the database connection
    pub fn db(&self) -> &Arc<Mutex<DbConnection>> {
        &self.db
    }

    // ========== Outgoing ==========

    /// Send content to a topic, resolving recipients from the database
    ///
    /// Looks up topic members, validates membership, filters out self,
    /// encodes as Content message, and delivers to all recipients.
    pub async fn send_content(
        &self,
        topic_id: &[u8; 32],
        payload: &[u8],
    ) -> Result<(), SendError> {
        let our_id = self.endpoint_id();

        // Get topic members with relay info
        let members_with_info = {
            let db = self.db.lock().await;
            get_topic_members_with_info(&db, topic_id)
                .map_err(|e| SendError::Database(e.to_string()))?
        };

        trace!(
            topic = %hex::encode(topic_id),
            member_count = members_with_info.len(),
            "sending message"
        );

        if members_with_info.is_empty() {
            return Err(SendError::TopicNotFound);
        }

        if !members_with_info.iter().any(|m| m.endpoint_id == our_id) {
            return Err(SendError::NotMember);
        }

        // Get recipients (all members except us)
        let recipients: Vec<MemberInfo> = members_with_info
            .into_iter()
            .filter(|m| m.endpoint_id != our_id)
            .map(|m| MemberInfo {
                endpoint_id: m.endpoint_id,
                relay_url: m.relay_url,
            })
            .collect();

        if recipients.is_empty() {
            trace!("no recipients - only member is self");
            return Ok(());
        }

        // Encode as Content message and send
        let content_msg = super::topic_messages::TopicMessage::Content(payload.to_vec());
        let encoded_payload = content_msg.encode();

        self.send_to_topic(topic_id, &encoded_payload, &recipients, HarborPacketType::Content)
            .await?;

        Ok(())
    }

    /// Send a message to topic members
    ///
    /// Lower-level entry point for all send operations.
    /// Takes pre-encoded TopicMessage bytes and delivers to all recipients.
    ///
    /// # Arguments
    /// * `topic_id` - The topic to send on
    /// * `encoded_payload` - Pre-encoded TopicMessage bytes (from TopicMessage::encode())
    /// * `recipients` - MemberInfo for topic members to send to
    /// * `packet_type` - Type of packet (Content, Join, Leave, etc.)
    pub async fn send_to_topic(
        &self,
        topic_id: &[u8; 32],
        encoded_payload: &[u8],
        recipients: &[MemberInfo],
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
            encoded_payload,
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

        // Filter out self and wildcard for actual delivery
        let our_id = self.identity.public_key;
        let actual_recipients: Vec<&MemberInfo> = recipients
            .iter()
            .filter(|m| m.endpoint_id != our_id && m.endpoint_id != WILDCARD_RECIPIENT)
            .collect();

        let mut delivered_to = Vec::new();
        let mut failed = Vec::new();

        // Mark self as delivered if in recipients
        if recipients.iter().any(|m| m.endpoint_id == our_id) {
            delivered_to.push(our_id);
        }

        if actual_recipients.is_empty() {
            debug!(packet_id = hex::encode(&packet_id[..8]), "no recipients (only self/wildcard)");
            return Ok(SendResult { packet_id, delivered_to, failed });
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            recipient_count = actual_recipients.len(),
            "sending to recipients in parallel"
        );

        // Deliver to all recipients in parallel via irpc RPC
        let send_futures = actual_recipients.iter().map(|member| {
            let packet_clone = packet.clone();
            let member = (*member).clone();
            async move {
                let result = self.deliver_to_member(&member, packet_clone).await;
                (member.endpoint_id, result)
            }
        });

        let results = join_all(send_futures).await;

        for (endpoint_id, result) in results {
            match result {
                Ok(receipt) => {
                    delivered_to.push(endpoint_id);
                    trace!(
                        recipient = hex::encode(endpoint_id),
                        receipt_from = hex::encode(receipt.sender),
                        "packet delivered with receipt"
                    );
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    failed.push((endpoint_id, err_msg.clone()));
                    debug!(
                        recipient = hex::encode(endpoint_id),
                        error = %err_msg,
                        "delivery failed"
                    );
                }
            }
        }

        info!(
            packet_id = hex::encode(&packet_id[..8]),
            delivered = delivered_to.len(),
            failed = failed.len(),
            "parallel send complete"
        );

        Ok(SendResult {
            packet_id,
            delivered_to,
            failed,
        })
    }

    // ========== Transport: irpc RPC delivery ==========

    /// Deliver a packet to a single member via irpc RPC
    ///
    /// Opens a connection, sends DeliverPacket RPC, receives Receipt inline.
    async fn deliver_to_member(
        &self,
        member: &MemberInfo,
        packet: crate::security::SendPacket,
    ) -> Result<Receipt, SendError> {
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

        // Send via irpc RPC - DeliverPacket request, Receipt response
        let client = irpc::Client::<SendRpcProtocol>::boxed(
            ExistingConnection::new(&conn)
        );
        let receipt = client
            .rpc(DeliverPacket { packet })
            .await
            .map_err(|e| SendError::Send(e.to_string()))?;

        Ok(receipt)
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

        // Create new connection
        let conn = tokio::time::timeout(
            self.send_timeout,
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

    /// Close connection to a node
    pub async fn close_connection(&self, node_id: EndpointId) {
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.remove(&node_id) {
            conn.close(0u32.into(), b"close");
        }
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
